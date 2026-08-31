// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Gateway configuration, deserialised from `config/default.yaml`.

use std::collections::HashMap;
use std::path::Path;

use sentin_core::{DataKind, Decision};
use serde::{Deserialize, Serialize};

/// The whole gateway configuration, as read from YAML.
///
/// Every section defaults, so a partial file is valid and an absent one yields a working gateway
/// with layer 1 only. That is deliberate: the failure mode of a strict parser here is a gateway
/// that will not start, in front of someone's actual work.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Address the gateway binds to.
    #[serde(default)]
    pub listen: Listen,
    /// Upstreams by adapter name — `anthropic`, `openai`, `google`.
    #[serde(default)]
    pub providers: HashMap<String, Provider>,
    /// Per-detector verdict ceiling. A detector missing from here defaults to `Observed`, so
    /// adding one in code cannot silently start blocking traffic.
    #[serde(default)]
    pub detectors: HashMap<String, DetectorRule>,
    /// Which side of the exchange is inspected, and how streams are handled.
    #[serde(default)]
    pub inspect: Inspect,
    /// Layer-2 model and device settings.
    #[serde(default)]
    pub inference: Inference,
    /// Audit sinks.
    #[serde(default)]
    pub audit: Audit,
}

/// Where the gateway listens.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Listen {
    /// Bind address. `127.0.0.1` by default — this is a local privacy gateway, and exposing it on
    /// a LAN is a decision an operator should have to make explicitly.
    pub host: String,
    /// Bind port. 4141, not 4000, which model routers such as LiteLLM commonly hold.
    pub port: u16,
}

impl Default for Listen {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            // Matches config/default.yaml. Not 4000 — model routers such as LiteLLM commonly
            // hold that port, and the fallback should not be the one address likely to be taken.
            port: 4141,
        }
    }
}

/// One upstream and the path prefix that routes to it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Provider {
    /// Path prefix the agent points at, e.g. `/anthropic`.
    pub prefix: String,
    /// Where matching requests are forwarded, e.g. `https://api.anthropic.com`.
    pub upstream: String,
}

/// Layer-2 inference settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Inference {
    /// `NPU`, `GPU`, `CPU` or `AUTO`. Resolved at load time; the device that actually ran is
    /// logged, because `AUTO` can pick something other than what the operator expected.
    #[serde(default = "default_device")]
    pub device: String,
    /// Directory holding the IR. Empty disables layer 2 entirely.
    #[serde(default)]
    pub model_dir: String,
    /// How long inspection may take before the timeout policy applies.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// What to do when it does not finish in time.
    #[serde(default)]
    pub timeout_policy: TimeoutPolicy,
    /// What `AUTO` optimises for once every device has been timed: `cost` or `latency`.
    ///
    /// Ignored when `device` names a device explicitly - an operator who pins a device gets it.
    #[serde(default = "default_select")]
    pub select: String,
    /// The measured steady-state inference a device must beat to be considered, in milliseconds.
    ///
    /// This is what stops `AUTO` selecting a device merely because it exists. Default 80 ms: the
    /// NPU budget from M2b, so a device that cannot hold the accelerator budget is not treated as
    /// an accelerator. Raise it on a machine where the CPU is the only option and is slower than
    /// this - the ceiling rejects, it does not disable layer 2.
    #[serde(default = "default_max_inference_ms")]
    pub max_inference_ms: f64,
}

impl Default for Inference {
    fn default() -> Self {
        Self {
            device: default_device(),
            model_dir: String::new(),
            timeout_ms: default_timeout_ms(),
            timeout_policy: TimeoutPolicy::default(),
            select: default_select(),
            max_inference_ms: default_max_inference_ms(),
        }
    }
}

impl Inference {
    /// Layer 2 runs only when a model directory is configured.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.model_dir.is_empty()
    }
}

fn default_device() -> String {
    "AUTO".to_string()
}

fn default_select() -> String {
    "cost".to_string()
}

fn default_max_inference_ms() -> f64 {
    80.0
}

fn default_timeout_ms() -> u64 {
    250
}

/// Where audit events go. Every sink is off by default except the local file: a gateway that
/// silently starts shipping events to a network collector nobody configured would be a surprise.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Audit {
    /// Local JSON Lines file. On by default: it needs no infrastructure.
    #[serde(default)]
    pub jsonl: JsonlSink,
    /// CEF over syslog, for a SIEM that speaks it.
    #[serde(default)]
    pub syslog_cef: SyslogSink,
    /// OTLP over HTTP with JSON encoding.
    #[serde(default)]
    pub otlp: OtlpSink,
}

/// Append-only JSON Lines audit file.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonlSink {
    /// Whether to write it. On by default.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Where to write it.
    #[serde(default = "default_audit_path")]
    pub path: String,
}

impl Default for JsonlSink {
    fn default() -> Self {
        Self {
            enabled: true,
            path: default_audit_path(),
        }
    }
}

fn default_audit_path() -> String {
    "./sentin-audit.jsonl".to_string()
}

/// CEF over syslog. Off unless configured, so the gateway never starts shipping events to a
/// collector nobody asked for.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SyslogSink {
    /// Whether to send.
    #[serde(default)]
    pub enabled: bool,
    /// Collector address, `host:port`.
    #[serde(default)]
    pub address: String,
}

/// OTLP over HTTP, JSON-encoded — which the spec permits, and which keeps protobuf codegen and a
/// gRPC stack out of the gateway.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OtlpSink {
    /// Whether to send.
    #[serde(default)]
    pub enabled: bool,
    /// Collector endpoint URL.
    #[serde(default)]
    pub endpoint: String,
}

/// What to do when inspection does not finish in time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutPolicy {
    /// Forward the request uninspected. The PoC default: the gateway sits in the path of real
    /// work, and a slow model must not become an outage.
    #[default]
    FailOpen,
    /// Refuse the request. Correct where policy demands inspection, at the cost of availability.
    FailClosed,
}

/// The operator's ceiling for one detector.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct DetectorRule {
    /// Strongest action this detector may request. Clamped again by the finding's own evidence,
    /// so configuring `block` for a pattern-only detector cannot actually block anything.
    pub mode: Decision,
}

/// Which side of the exchange is inspected.
///
/// `response` defaults to off pending research question B2 — see [`StreamStrategy`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Inspect {
    /// Inspect outbound requests. This is the threat model — data leaving the device — and it
    /// costs +0.07 ms, so it is on by default.
    #[serde(default = "default_true")]
    pub request: bool,
    /// Inspect responses. Findings are detected and audited; masking a stream mid-render is
    /// roadmap, not PoC.
    #[serde(default)]
    pub response: bool,
    /// How a streamed response is inspected.
    #[serde(default)]
    pub stream_strategy: StreamStrategy,
}

impl Default for Inspect {
    fn default() -> Self {
        Self {
            request: true,
            response: false,
            stream_strategy: StreamStrategy::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// How a streaming (SSE) response is inspected. This is the subject of research question B2.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamStrategy {
    /// Forward chunks untouched. Request-side inspection only. Zero added time to first token.
    #[default]
    Passthrough,
    /// Accumulate the whole response, inspect once, then emit. Safest, worst latency: the client
    /// sees nothing until the model has finished.
    Buffer,
    /// Inspect on sentence boundaries, releasing text as each boundary is passed.
    SlidingWindow,
}

/// Why a configuration file could not be used. Both variants are startup errors: an unreadable or
/// malformed config is worth refusing to start over, unlike a missing model.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("reading {path}: {source}")]
    Io {
        /// The path that was tried.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid YAML, or does not match the schema.
    #[error("parsing {path}: {source}")]
    Parse {
        /// The path that was tried.
        path: String,
        /// What the YAML parser reported, including the line.
        #[source]
        source: serde_yaml_ng::Error,
    },
}

impl Config {
    /// Load configuration from a YAML file.
    ///
    /// # Errors
    /// Returns an error when the file cannot be read or is not valid configuration YAML.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        serde_yaml_ng::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    /// The action configured for a detector, defaulting to `Observed` for unknown kinds.
    ///
    /// Defaulting to the weakest action matters: a detector added in code but not yet in the
    /// config must not silently start blocking traffic.
    #[must_use]
    pub fn mode_for(&self, kind: DataKind) -> Decision {
        self.detectors
            .get(detector_key(kind))
            .map_or(Decision::Observed, |rule| rule.mode)
    }

    /// Find the provider whose prefix matches this request path.
    #[must_use]
    pub fn provider_for<'a>(&'a self, path: &str) -> Option<(&'a str, &'a Provider)> {
        self.providers
            .iter()
            .find(|(_, provider)| path.starts_with(&provider.prefix))
            .map(|(name, provider)| (name.as_str(), provider))
    }
}

/// Config key for a detector. Kept next to `DataKind` so the two cannot drift silently.
#[must_use]
pub fn detector_key(kind: DataKind) -> &'static str {
    match kind {
        DataKind::Pesel => "pesel",
        DataKind::Nip => "nip",
        DataKind::Regon => "regon",
        DataKind::Iban => "iban",
        DataKind::PaymentCard => "payment_card",
        DataKind::Email => "email",
        DataKind::PhonePl => "phone_pl",
        DataKind::Person => "person",
        DataKind::Organization => "organization",
        DataKind::Location => "location",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_data_kind_has_a_config_key() {
        // A kind without a key would be unconfigurable and silently stuck on the default.
        let kinds = [
            DataKind::Pesel,
            DataKind::Nip,
            DataKind::Regon,
            DataKind::Iban,
            DataKind::PaymentCard,
            DataKind::Email,
            DataKind::PhonePl,
            DataKind::Person,
            DataKind::Organization,
            DataKind::Location,
        ];
        let keys: std::collections::HashSet<_> = kinds.iter().map(|k| detector_key(*k)).collect();
        assert_eq!(keys.len(), kinds.len(), "detector keys must be unique");
    }

    #[test]
    fn shipped_default_config_parses_and_covers_every_detector() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/default.yaml");
        let config = Config::load(path).expect("shipped config must parse");

        // Not 4000: model routers such as LiteLLM habitually own that port, and a default the
        // gateway cannot bind to is a bad first run. Asserted so the two cannot drift apart.
        assert_eq!(config.listen.port, 4141);
        assert_ne!(
            config.listen.port, 4000,
            "4000 belongs to the router, not to us"
        );
        assert!(config.providers.contains_key("anthropic"));
        assert!(config.inspect.request, "request inspection is the PoC core");

        for kind in [
            DataKind::Pesel,
            DataKind::Nip,
            DataKind::Regon,
            DataKind::Iban,
            DataKind::PaymentCard,
            DataKind::Email,
            DataKind::PhonePl,
        ] {
            assert!(
                config.detectors.contains_key(detector_key(kind)),
                "{kind:?} missing from shipped config"
            );
        }
    }

    #[test]
    fn unknown_detectors_default_to_the_weakest_action() {
        let config: Config = serde_yaml_ng::from_str("detectors: {}").expect("parses");
        assert_eq!(config.mode_for(DataKind::Pesel), Decision::Observed);
    }
}
