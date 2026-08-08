// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! End-to-end: a client sends a request through the gateway to a provider.
//!
//! The assertion that matters is not "the gateway returned 200" but **what reached the upstream**.
//! The mock records every body it receives, so these tests can prove that the masked text crossed
//! the boundary and the original identifier did not. A gateway that masks the copy it shows the
//! user while forwarding the original would pass a weaker test and fail its entire purpose.

use sentin_detect::testdata;
use sentin_proxy::config::Config;
use sentin_proxy::mock::{self, Received, StreamShape};
use sentin_proxy::AppState;
use serde_json::{json, Value};

/// Start a mock upstream and a gateway wired to it. Returns the gateway base URL and the recorder.
async fn start(detector_mode: &str, extra: &str) -> (String, Received) {
    let (upstream, received) = mock::spawn(StreamShape {
        events: 4,
        gap_ms: 0,
    })
    .await;

    let yaml = format!(
        "providers:\n  \
           openai:\n    prefix: /openai\n    upstream: {upstream}\n  \
           anthropic:\n    prefix: /anthropic\n    upstream: {upstream}\n  \
           google:\n    prefix: /google\n    upstream: {upstream}\n\
         detectors:\n  \
           pesel: {{ mode: {detector_mode} }}\n  \
           email: {{ mode: {detector_mode} }}\n  \
           iban: {{ mode: {detector_mode} }}\n\
         {extra}"
    );
    let config: Config = serde_yaml_ng::from_str(&yaml).expect("test config parses");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind gateway");
    let address = listener.local_addr().expect("gateway addr");
    tokio::spawn(async move {
        let _ = sentin_proxy::serve(listener, AppState::new(config)).await;
    });

    (format!("http://{address}"), received)
}

async fn post(url: &str, body: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(url)
        .header("content-type", "application/json")
        .header("x-api-key", "sk-test-secret")
        .json(body)
        .send()
        .await
        .expect("request reaches the gateway")
}

#[tokio::test]
async fn masked_text_reaches_the_upstream_and_the_original_does_not() {
    let (gateway, received) = start("mask", "").await;
    let pesel = testdata::pesel(1944, 5, 14, 135);

    let response = post(
        &format!("{gateway}/openai/v1/chat/completions"),
        &json!({"model": "gpt-4o", "messages": [
            {"role": "user", "content": format!("Zweryfikuj PESEL {pesel} proszę.")}
        ]}),
    )
    .await;

    assert_eq!(response.status(), 200, "the caller still gets an answer");

    let upstream_saw = received.all_text();
    assert!(
        !upstream_saw.contains(&pesel),
        "the raw PESEL left the machine: {upstream_saw}"
    );
    assert!(
        upstream_saw.contains("[PESEL]"),
        "expected a placeholder upstream, got: {upstream_saw}"
    );
    assert!(
        upstream_saw.contains("Zweryfikuj"),
        "surrounding text must survive masking"
    );
}

#[tokio::test]
async fn blocked_requests_never_reach_the_upstream() {
    let (gateway, received) = start("block", "").await;
    let pesel = testdata::pesel(1985, 1, 1, 1234);

    let response = post(
        &format!("{gateway}/openai/v1/chat/completions"),
        &json!({"messages": [{"role": "user", "content": pesel}]}),
    )
    .await;

    assert_eq!(response.status(), 403);
    let body: Value = response.json().await.expect("json error body");
    assert_eq!(body["error"]["type"], "sentin_policy_block");
    assert_eq!(body["error"]["detected"][0], "pesel");
    // The refusal names the data kind, never the value.
    assert!(!body.to_string().contains(&pesel));

    assert!(
        received.all().is_empty(),
        "a blocked request must not be forwarded at all"
    );
}

#[tokio::test]
async fn clean_requests_pass_through_untouched() {
    let (gateway, received) = start("mask", "").await;

    let response = post(
        &format!("{gateway}/openai/v1/chat/completions"),
        &json!({"model": "gpt-4o", "temperature": 0.3, "messages": [
            {"role": "user", "content": "Jaka jest pogoda w Krakowie?"}
        ]}),
    )
    .await;

    assert_eq!(response.status(), 200);
    let forwarded = received.last().expect("upstream received the request");
    assert_eq!(
        forwarded["messages"][0]["content"],
        "Jaka jest pogoda w Krakowie?"
    );
    // Fields the gateway does not understand must survive the round trip unchanged.
    assert_eq!(forwarded["temperature"], 0.3);
    assert_eq!(forwarded["model"], "gpt-4o");
}

#[tokio::test]
async fn all_three_provider_schemas_are_masked() {
    let (gateway, received) = start("mask", "").await;
    let pesel = testdata::pesel(1944, 5, 14, 135);

    let cases: [(&str, Value); 3] = [
        (
            "/anthropic/v1/messages",
            json!({"messages": [{"role": "user", "content": [
                {"type": "text", "text": format!("PESEL: {pesel}")}
            ]}]}),
        ),
        (
            "/openai/v1/chat/completions",
            json!({"messages": [{"role": "user", "content": format!("PESEL: {pesel}")}]}),
        ),
        (
            "/google/v1beta/models/gemini",
            json!({"contents": [{"role": "user", "parts": [{"text": format!("PESEL: {pesel}")}]}]}),
        ),
    ];

    for (path, body) in cases {
        let response = post(&format!("{gateway}{path}"), &body).await;
        assert_eq!(response.status(), 200, "{path}");
        let forwarded = received.last().expect("upstream received it").to_string();
        assert!(!forwarded.contains(&pesel), "{path} leaked the PESEL");
        assert!(forwarded.contains("[PESEL]"), "{path} was not masked");
    }
}

#[tokio::test]
async fn the_api_key_is_forwarded_to_the_upstream() {
    // The gateway is a proxy, not a credential broker: the caller's key must arrive intact or
    // their billing and rate limits break.
    let (upstream, _) = mock::spawn(StreamShape::default()).await;
    let yaml = format!(
        "providers:\n  openai:\n    prefix: /openai\n    upstream: {upstream}\ndetectors: {{}}\n"
    );
    let config: Config = serde_yaml_ng::from_str(&yaml).expect("config parses");

    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-api-key", "sk-test-secret".parse().unwrap());
    let forwarded = sentin_proxy::forwardable_headers(&headers);
    assert_eq!(forwarded.get("x-api-key").unwrap(), "sk-test-secret");
    assert!(!sentin_proxy::loggable_header_names(&headers).contains(&"x-api-key".to_string()));
    drop(config);
}

#[tokio::test]
async fn streaming_responses_are_relayed_intact() {
    let (gateway, _) = start(
        "mask",
        "inspect:\n  request: true\n  stream_strategy: passthrough\n",
    )
    .await;

    let response = post(
        &format!("{gateway}/openai/v1/chat/completions"),
        &json!({"stream": true, "messages": [{"role": "user", "content": "opowiedz"}]}),
    )
    .await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream"),
        "SSE content type must survive the relay"
    );

    let body = response.text().await.expect("stream body");
    assert_eq!(body.matches("data: ").count(), 4, "all events relayed");
}

#[tokio::test]
async fn every_stream_strategy_delivers_the_same_bytes() {
    // Whatever B2 decides, the strategies must differ only in *when* bytes arrive, never in what
    // arrives. A strategy that drops or reorders an event would corrupt the client's parser.
    let mut bodies = Vec::new();
    for strategy in ["passthrough", "buffer", "sliding_window"] {
        let (gateway, _) = start(
            "mask",
            &format!("inspect:\n  request: true\n  stream_strategy: {strategy}\n"),
        )
        .await;

        let response = post(
            &format!("{gateway}/openai/v1/chat/completions"),
            &json!({"stream": true, "messages": [{"role": "user", "content": "opowiedz"}]}),
        )
        .await;
        bodies.push((strategy, response.text().await.expect("stream body")));
    }

    let (_, reference) = &bodies[0];
    for (strategy, body) in &bodies {
        assert_eq!(body, reference, "{strategy} changed the response bytes");
        assert_eq!(body.matches("data: ").count(), 4, "{strategy} lost events");
    }
}

#[tokio::test]
async fn unparseable_bodies_are_forwarded_rather_than_rejected() {
    // An unsupported body shape must not break the caller's request; the gateway is in the path
    // of real work and failing closed on a parse error would be worse than not inspecting.
    let (gateway, _) = start("mask", "").await;

    let response = reqwest::Client::new()
        .post(format!("{gateway}/openai/v1/chat/completions"))
        .header("content-type", "application/json")
        .body("this is not json at all")
        .send()
        .await
        .expect("request reaches the gateway");

    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn unknown_paths_are_refused_clearly() {
    let (gateway, _) = start("mask", "").await;
    let response = reqwest::Client::new()
        .post(format!("{gateway}/unknown/v1/thing"))
        .json(&json!({}))
        .send()
        .await
        .expect("request reaches the gateway");
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn audit_events_are_written_and_never_contain_the_detected_value() {
    // The claim this project makes to a SOC is that the trail is safe to ingest. That is only
    // true if it is checked against a real request, on the real path, with a real identifier.
    let (upstream, _received) = mock::spawn(StreamShape {
        events: 2,
        gap_ms: 0,
    })
    .await;

    let audit_path =
        std::env::temp_dir().join(format!("sentin-audit-e2e-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&audit_path);

    let yaml = format!(
        "providers:\n  openai:\n    prefix: /openai\n    upstream: {upstream}\n\
         detectors:\n  pesel: {{ mode: mask }}\n  email: {{ mode: advise }}\n\
         audit:\n  jsonl:\n    enabled: true\n    path: {}\n",
        audit_path.display()
    );
    let config: Config = serde_yaml_ng::from_str(&yaml).expect("config parses");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = sentin_proxy::serve(listener, AppState::new(config)).await;
    });

    let pesel = testdata::pesel(1944, 5, 14, 135);
    let response = post(
        &format!("http://{address}/openai/v1/chat/completions"),
        &json!({"messages": [{"role": "user",
            "content": format!("PESEL {pesel}, kontakt biuro@firma.pl")}]}),
    )
    .await;
    assert_eq!(response.status(), 200);

    // The sink writes synchronously on the request path, but give the OS a moment regardless.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let contents = std::fs::read_to_string(&audit_path).expect("audit file was created");
    let lines: Vec<&str> = contents.lines().filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty(), "the request produced no audit events");

    let mut kinds = Vec::new();
    let mut decisions = Vec::new();
    for line in &lines {
        let event: Value = serde_json::from_str(line).expect("each line is one JSON object");
        assert!(
            !line.contains(&pesel),
            "an audit event quoted the detected PESEL: {line}"
        );
        assert!(
            !line.contains("biuro@firma.pl"),
            "an audit event quoted the detected email: {line}"
        );
        if let Some(kind) = event["data_type"].as_str() {
            kinds.push(kind.to_string());
        }
        if let Some(decision) = event["decision"].as_str() {
            decisions.push(decision.to_string());
        }
        assert!(
            event["content_sha256"]
                .as_str()
                .is_some_and(|h| h.starts_with("sha256:")),
            "every event needs the correlating hash: {line}"
        );
    }

    assert!(kinds.contains(&"PESEL".to_string()), "kinds were {kinds:?}");
    assert!(kinds.contains(&"EMAIL".to_string()), "kinds were {kinds:?}");
    assert!(
        decisions.contains(&"masked".to_string()),
        "decisions were {decisions:?}"
    );
    // One decision_made summarising the request, alongside the per-finding events.
    assert!(
        contents.contains("decision_made"),
        "expected a decision_made event: {contents}"
    );

    let _ = std::fs::remove_file(&audit_path);
}

#[tokio::test]
async fn a_clean_request_produces_no_audit_noise() {
    // A SIEM full of events about nothing is worse than no SIEM: real signals get lost in it.
    let (upstream, _received) = mock::spawn(StreamShape::default()).await;
    let audit_path =
        std::env::temp_dir().join(format!("sentin-audit-clean-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&audit_path);

    let yaml = format!(
        "providers:\n  openai:\n    prefix: /openai\n    upstream: {upstream}\n\
         detectors:\n  pesel: {{ mode: mask }}\n\
         audit:\n  jsonl:\n    enabled: true\n    path: {}\n",
        audit_path.display()
    );
    let config: Config = serde_yaml_ng::from_str(&yaml).expect("config parses");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = sentin_proxy::serve(listener, AppState::new(config)).await;
    });

    post(
        &format!("http://{address}/openai/v1/chat/completions"),
        &json!({"messages": [{"role": "user", "content": "Jaka jest pogoda w Krakowie?"}]}),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let contents = std::fs::read_to_string(&audit_path).unwrap_or_default();
    assert!(
        contents.trim().is_empty(),
        "a request with no findings must produce no events, got: {contents}"
    );
    let _ = std::fs::remove_file(&audit_path);
}
