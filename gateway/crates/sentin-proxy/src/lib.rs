// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! The gateway proxy: provider adapters, the inspection hook, and SSE streaming (Phase 3).
//!
//! Routes `/anthropic/*`, `/openai/*` and `/google/*` to their upstreams. The caller's API key is
//! forwarded verbatim and **never logged** — see [`forwardable_headers`], which is the single
//! place that decides what crosses the boundary.

#![warn(missing_docs)]

pub mod adapters;
pub mod audit_sink;
pub mod config;

/// Re-exported so existing callers keep working after diagnostics moved to their own crate.
pub use sentin_diag::{doctor, energy, fingerprint};
pub mod inspect;
pub mod mock;
pub mod ner_service;
pub mod otlp;
/// Running as a Windows service. Present only on Windows, where a gateway nobody remembers to
/// start is a gateway that is sometimes not inspecting.
#[cfg(windows)]
pub mod service;
pub mod stream;

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use sentin_core::Decision;
use serde_json::Value;

use crate::adapters::Provider;
use crate::config::Config;
use crate::inspect::inspect_request;

/// Headers that belong to one hop and must not be relayed.
const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// Headers whose values are secrets. They are forwarded, but never appear in a log line.
const SECRET_HEADERS: [&str; 3] = ["authorization", "x-api-key", "x-goog-api-key"];

/// Everything a request handler needs, cloned per request.
#[derive(Clone, Debug)]
pub struct AppState {
    /// The parsed configuration, shared rather than copied.
    pub config: Arc<Config>,
    /// The outbound HTTP client, reused so connections are pooled across requests.
    pub client: reqwest::Client,
    /// Layer 2, when a model is configured and loads. `None` means layer 1 only — a missing or
    /// broken model degrades the gateway rather than stopping it.
    pub ner: Option<Arc<crate::ner_service::NerService>>,
    /// Audit sinks. Always present, possibly empty.
    pub audit: Arc<sentin_audit::emit::Fanout>,
}

impl AppState {
    /// Build state without layer 2.
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self::with_ner(config, None)
    }

    /// Build state, starting layer 2 if the configuration enables it.
    ///
    /// A model that will not load is logged and skipped, not fatal: layer 1 catches structured
    /// identifiers on its own, and refusing to start would turn a model problem into an outage.
    #[must_use]
    pub fn with_inference(config: Config) -> Self {
        let ner = if config.inference.is_enabled() {
            match crate::ner_service::NerService::start(&config.inference) {
                Ok(service) => {
                    tracing::info!(
                        device = %service.device(),
                        fell_back = service.fell_back(),
                        policy = ?service.policy(),
                        selection = service.selection().unwrap_or("pinned by configuration"),
                        "layer 2 ready"
                    );
                    Some(Arc::new(service))
                }
                Err(err) => {
                    tracing::warn!(error = %err, "layer 2 unavailable; continuing with layer 1");
                    None
                }
            }
        } else {
            None
        };
        Self::with_ner(config, ner)
    }

    /// Build the state with a layer-2 service already constructed.
    ///
    /// Separate from [`AppState::new`] so tests can inject a service, or deliberately run without
    /// one, without the loading path being involved.
    #[must_use]
    pub fn with_ner(config: Config, ner: Option<Arc<crate::ner_service::NerService>>) -> Self {
        let audit = crate::audit_sink::build(&config.audit, env!("CARGO_PKG_VERSION"));
        if !audit.is_empty() {
            tracing::info!(sinks = ?audit.names(), "audit enabled");
        }
        Self {
            ner,
            audit,
            config: Arc::new(config),
            // No timeout here: streamed completions legitimately run for minutes. Inspection has
            // its own timeout; the upstream call does not get to be the thing that gives up.
            client: reqwest::Client::builder()
                .build()
                .expect("HTTP client construction cannot fail with default TLS"),
        }
    }
}

/// Build the router. Every path is handled by one proxy function, which dispatches on prefix.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", any(|| async { "ok" }))
        .fallback(any(proxy))
        .with_state(state)
}

/// Headers to relay upstream: everything except hop-by-hop and `host`.
///
/// `authorization` and `x-api-key` deliberately pass through — the gateway is a proxy, not a
/// credential broker, and rewriting them would break the caller's own billing and rate limits.
#[must_use]
pub fn forwardable_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let lower = name.as_str().to_ascii_lowercase();
        if HOP_BY_HOP.contains(&lower.as_str()) || lower == "host" || lower == "content-length" {
            continue;
        }
        out.insert(name.clone(), value.clone());
    }
    out
}

/// Header names safe to include in a log line. Used by tests to prove secrets are excluded.
#[must_use]
pub fn loggable_header_names(headers: &HeaderMap) -> Vec<String> {
    headers
        .keys()
        .map(|name| name.as_str().to_ascii_lowercase())
        .filter(|name| !SECRET_HEADERS.contains(&name.as_str()))
        .collect()
}

async fn proxy(State(state): State<AppState>, request: Request) -> Response {
    let started = Instant::now();
    let path = request.uri().path().to_string();
    let query = request.uri().query().map(str::to_string);
    // Read from the extensions rather than as an extractor: `ConnectInfo` is only present when the
    // server was built with it, and the e2e tests drive the router directly. A missing address is
    // recorded as missing, never guessed from a header a caller controls.
    let client_addr = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        // The address without the port. The port is ephemeral - a new one per request - so an
        // event carrying it cannot be grouped by caller, which is the one thing a SIEM wants to do
        // with this field. Found by a Wazuh frequency rule that could never fire: eight requests
        // from one workstation looked like eight different callers.
        .map(|info| info.0.ip().to_string());

    let Some((provider_name, provider_config)) = state.config.provider_for(&path) else {
        return error(
            StatusCode::NOT_FOUND,
            "no provider configured for this path",
        );
    };
    let provider_name = provider_name.to_string();
    let upstream_base = provider_config.upstream.clone();
    let prefix = provider_config.prefix.clone();

    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => return error(StatusCode::BAD_REQUEST, "could not read request body"),
    };

    // Inspection applies to JSON bodies we can parse. Anything else is forwarded untouched rather
    // than rejected: an unsupported shape must not break the caller's request.
    let mut outgoing = bytes.clone();
    let mut verdict = inspect::Inspection::clean();

    if state.config.inspect.request && !bytes.is_empty() {
        if let (Some(schema), Ok(json)) = (
            Provider::from_name(&provider_name),
            serde_json::from_slice::<Value>(&bytes),
        ) {
            verdict = inspect_request(&json, schema, &state.config, state.ner.as_deref()).await;

            // Layer 2 not contributing is an operator decision, not inspection's: fail-open
            // forwards with a warning, fail-closed refuses.
            if let Some(reason) = &verdict.ner_skipped {
                match state.config.inference.timeout_policy {
                    crate::config::TimeoutPolicy::FailOpen => tracing::warn!(
                        ?reason,
                        "layer 2 skipped; forwarding on layer 1 only (fail-open)"
                    ),
                    crate::config::TimeoutPolicy::FailClosed => {
                        tracing::warn!(?reason, "layer 2 skipped; refusing (fail-closed)");
                        return error(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "inspection could not complete and policy is fail-closed",
                        );
                    }
                }
            }

            let host = upstream_host(&upstream_base);
            crate::audit_sink::record_request(
                &state.audit,
                &verdict,
                &bytes,
                &crate::audit_sink::RequestContext {
                    target_host: &host,
                    model_id: model_id(&state.config.inference.model_dir),
                    device: state.ner.as_ref().map(|n| n.device()),
                    client_addr: client_addr.as_deref(),
                    upstream_model: upstream_model(&json, &path).as_deref(),
                    provider: &provider_name,
                },
            );

            if verdict.decision == Decision::Blocked {
                tracing::info!(
                    provider = %provider_name,
                    findings = %verdict.summary(),
                    "request blocked"
                );
                return blocked_response(&verdict);
            }
            if let Some(masked) = &verdict.masked_body {
                match serde_json::to_vec(masked) {
                    Ok(encoded) => outgoing = encoded.into(),
                    // Failing to re-encode must not silently forward the unmasked original.
                    Err(_) => {
                        return error(StatusCode::INTERNAL_SERVER_ERROR, "masking failed");
                    }
                }
            }
        }
    }

    let suffix = path.strip_prefix(&prefix).unwrap_or(&path);
    let mut url = format!("{}{}", upstream_base.trim_end_matches('/'), suffix);
    if let Some(query) = query {
        url.push('?');
        url.push_str(&query);
    }

    let upstream = state
        .client
        .request(parts.method.clone(), &url)
        .headers(forwardable_headers(&parts.headers))
        .body(outgoing)
        .send()
        .await;

    let response = match upstream {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!(provider = %provider_name, error = %err, "upstream request failed");
            return error(StatusCode::BAD_GATEWAY, "upstream request failed");
        }
    };

    tracing::info!(
        provider = %provider_name,
        status = response.status().as_u16(),
        findings = %verdict.summary(),
        decision = ?verdict.decision,
        elapsed_us = started.elapsed().as_micros() as u64,
        "proxied"
    );

    relay(response, &state, verdict.decision).await
}

/// Relay the upstream response, streaming it through the configured inspection strategy.
async fn relay(response: reqwest::Response, state: &AppState, decision: Decision) -> Response {
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    let mut builder = Response::builder().status(status);
    for (name, value) in response.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if HOP_BY_HOP.contains(&lower.as_str()) || lower == "content-length" {
            continue;
        }
        builder = builder.header(name.clone(), value.clone());
    }
    if decision == Decision::Advised {
        // Advisory mode is only meaningful if the caller can see the advice.
        builder = builder.header(
            HeaderName::from_static("x-sentin-advisory"),
            "sensitive-data-detected",
        );
    }

    let body = stream::relay_body(response, state.config.inspect.stream_strategy);
    builder.body(body).unwrap_or_else(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to build response",
        )
    })
}

/// Host part of an upstream URL. Audit events record the host, never the full URL: a query string
/// can carry content, and an audit trail that quoted it would defeat its own purpose.
fn upstream_host(upstream: &str) -> String {
    let without_scheme = upstream
        .split_once("://")
        .map_or(upstream, |(_, rest)| rest);
    without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .to_string()
}

/// The upstream model the caller asked for, for the audit trail.
///
/// Two providers put it in two places. OpenAI and Anthropic carry `"model"` in the body; Google
/// puts it in the path, as `/v1beta/models/gemini-2.5-pro:generateContent`. Both are read, because
/// "which model was this data heading for" is the question a SOC asks first and it must not depend
/// on which vendor the caller happened to use.
///
/// Returns `None` rather than a guess when neither shape matches - an absent field is honest, an
/// invented one is not.
fn upstream_model(body: &Value, path: &str) -> Option<String> {
    if let Some(model) = body.get("model").and_then(Value::as_str) {
        if !model.is_empty() {
            return Some(model.to_string());
        }
    }
    // Google: .../models/<name>:<method>. Take the segment after `models/`, up to the colon.
    let after = path.split("/models/").nth(1)?;
    let name = after.split(':').next().unwrap_or(after).trim_matches('/');
    (!name.is_empty()).then(|| name.to_string())
}

/// The `model_id` an audit event carries: the last component of the model directory.
///
/// Split on both separators, not just `/`. On Windows the configured path is
/// `D:\...\models\seq128`, so splitting on `/` alone returns the **whole path** - which is what a
/// first run on Windows put into every event on 2026-08-31, sending a local directory layout to
/// the SIEM in a field a parser expects to be a short identifier.
///
/// The field remains weak by design and is documented as such in `docs/events.md`: it reports the
/// shape (`seq128`), not the model.
fn model_id(model_dir: &str) -> Option<&str> {
    model_dir.rsplit(['/', '\\']).find(|part| !part.is_empty())
}

/// A refusal carries the data kinds involved, never the text that triggered it.
fn blocked_response(verdict: &inspect::Inspection) -> Response {
    let kinds: Vec<_> = verdict
        .findings
        .iter()
        .filter(|f| f.decision == Decision::Blocked)
        .map(|f| config::detector_key(f.kind))
        .collect();

    let body = serde_json::json!({
        "error": {
            "type": "sentin_policy_block",
            "message": "Request blocked by local policy: sensitive data detected on this device.",
            "detected": kinds,
        }
    });
    (StatusCode::FORBIDDEN, axum::Json(body)).into_response()
}

fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        axum::Json(serde_json::json!({"error": {"type": "sentin_gateway", "message": message}})),
    )
        .into_response()
}

/// Convenience for tests and the binary: bind and serve until the future is dropped.
///
/// # Errors
/// Returns an error if the listener cannot bind or the server stops unexpectedly.
pub async fn serve_with_shutdown<F>(
    listener: tokio::net::TcpListener,
    state: AppState,
    shutdown: F,
) -> std::io::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    // Graceful: stop accepting, let in-flight requests finish. The traffic passing through is
    // somebody's real work, and cutting a streamed completion in half to shave a second off a
    // restart is a poor trade.
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

/// Convenience for tests and the binary: bind and serve until the future is dropped.
///
/// # Errors
/// Returns an error if the listener cannot bind or the server stops unexpectedly.
pub async fn serve(listener: tokio::net::TcpListener, state: AppState) -> std::io::Result<()> {
    // `with_connect_info` rather than `into_make_service`: without it the peer address is not
    // available to the handler at all, and an audit trail that cannot say which workstation sent
    // a request answers "what happened" but never "to whom to talk about it".
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
}

/// Unused re-export kept so downstream code can build a body without importing axum directly.
pub type ProxyBody = Body;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn the_upstream_model_is_read_from_the_body_or_the_google_path() {
        let body: Value = serde_json::json!({"model": "ovh-llama", "messages": []});
        assert_eq!(
            upstream_model(&body, "/openai/v1/chat/completions").as_deref(),
            Some("ovh-llama")
        );

        // Google names the model in the path, not the body.
        let empty: Value = serde_json::json!({"contents": []});
        assert_eq!(
            upstream_model(
                &empty,
                "/google/v1beta/models/gemini-2.5-pro:generateContent"
            )
            .as_deref(),
            Some("gemini-2.5-pro")
        );

        // Nothing to read is recorded as nothing, not as a guess.
        assert_eq!(upstream_model(&empty, "/openai/v1/embeddings"), None);
        let blank: Value = serde_json::json!({"model": ""});
        assert_eq!(upstream_model(&blank, "/openai/v1/chat/completions"), None);
    }

    #[test]
    fn model_id_is_the_last_path_component_on_both_platforms() {
        assert_eq!(model_id("models/herbert/int8/seq128"), Some("seq128"));
        // The Windows case that shipped a whole local path to the SIEM before it was fixed.
        assert_eq!(
            model_id(r"D:\git_v2\Sentin-NPU\dist\bundle\models\seq128"),
            Some("seq128")
        );
        assert_eq!(
            model_id("models/seq512/"),
            Some("seq512"),
            "a trailing separator is not an id"
        );
        assert_eq!(model_id(""), None);
    }

    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("sk-secret-value"));
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer sk-another-secret"),
        );
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        headers.insert("connection", HeaderValue::from_static("keep-alive"));
        headers.insert("host", HeaderValue::from_static("localhost:4000"));
        headers
    }

    #[test]
    fn api_keys_are_forwarded_upstream() {
        // The gateway proxies; it does not hold or substitute credentials.
        let forwarded = forwardable_headers(&headers());
        assert_eq!(
            forwarded.get("x-api-key").map(|v| v.to_str().unwrap()),
            Some("sk-secret-value")
        );
        assert!(forwarded.contains_key("authorization"));
    }

    #[test]
    fn hop_by_hop_and_host_headers_are_dropped() {
        let forwarded = forwardable_headers(&headers());
        assert!(!forwarded.contains_key("connection"));
        assert!(!forwarded.contains_key("host"));
        assert!(forwarded.contains_key("content-type"));
    }

    #[test]
    fn secret_headers_are_never_loggable() {
        let names = loggable_header_names(&headers());
        assert!(!names.contains(&"x-api-key".to_string()));
        assert!(!names.contains(&"authorization".to_string()));
        assert!(names.contains(&"content-type".to_string()));
    }
}
