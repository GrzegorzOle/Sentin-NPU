// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Gateway configuration, deserialised from `config/default.yaml`.

use std::collections::HashMap;
use std::path::Path;

use sentin_core::{DataKind, Decision};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub listen: Listen,
    #[serde(default)]
    pub providers: HashMap<String, Provider>,
    #[serde(default)]
    pub detectors: HashMap<String, DetectorRule>,
    #[serde(default)]
    pub inspect: Inspect,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Listen {
    pub host: String,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Provider {
    pub prefix: String,
    pub upstream: String,
}

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
    #[serde(default = "default_true")]
    pub request: bool,
    #[serde(default)]
    pub response: bool,
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

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: String,
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
