// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Shared vocabulary for the Sentin-NPU inspection pipeline.
//!
//! This crate deliberately owns the types that encode the project's invariants, so that the
//! detection, proxy and audit crates cannot drift apart on them. In particular it encodes the
//! *advisory-first* rule: only the deterministic layer, on a checksum-valid match, may block.

use serde::{Deserialize, Serialize};

/// Which detection layer produced a finding.
///
/// The layer determines the strongest verdict the policy engine is allowed to reach — see
/// [`Layer::max_decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// L1: regex + checksum. Deterministic, and the only layer permitted to block.
    Deterministic,
    /// L2: NER inference (OpenVINO). Probabilistic — advises or masks, never blocks.
    Ner,
    /// L3: corporate policy artifacts. Out of PoC scope; reserved.
    Policy,
}

impl Layer {
    /// The strongest decision this layer may produce.
    ///
    /// This is the enforcement point for the advisory-first invariant. A layer that wants to
    /// escalate beyond its ceiling is a bug, not a configuration option.
    #[must_use]
    pub fn max_decision(self) -> Decision {
        match self {
            Layer::Deterministic => Decision::Blocked,
            Layer::Ner | Layer::Policy => Decision::Masked,
        }
    }
}

/// What the pipeline decided to do about a request.
///
/// Ordered from least to most restrictive; `Ord` reflects that ordering so a decision can be
/// clamped against [`Layer::max_decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Logged only; the request passes through untouched.
    Observed,
    /// The user was warned; the request passes through.
    Advised,
    /// Sensitive spans were replaced before forwarding.
    Masked,
    /// The request was refused. Reachable only from [`Layer::Deterministic`].
    Blocked,
}

/// The class of sensitive data a finding refers to.
///
/// Serialised into audit events as `data_type`. Never accompanied by the matched text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DataKind {
    Pesel,
    Nip,
    Regon,
    Iban,
    PaymentCard,
    Email,
    PhonePl,
    Person,
    Organization,
    Location,
}

/// A single detection: where it is, what it is, and which layer found it.
///
/// `span` is a byte range into the *original* text. Layer 2 must map model-tokenizer offsets back
/// to these coordinates before constructing a finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub span: std::ops::Range<usize>,
    pub kind: DataKind,
    /// 1.0 for checksum-validated deterministic matches; model score for NER.
    pub confidence: f32,
    pub layer: Layer,
}

impl Finding {
    /// Clamp a proposed decision to what this finding's layer is allowed to reach.
    #[must_use]
    pub fn clamp_decision(&self, proposed: Decision) -> Decision {
        proposed.min(self.layer.max_decision())
    }
}

/// Inference device, as requested by configuration.
///
/// The dev machine has an AMD NPU that OpenVINO cannot drive, so `Npu` is only ever satisfied on
/// the Intel test machines; everything else resolves through [`Device::Auto`] with fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Device {
    Npu,
    Gpu,
    Cpu,
    /// Try NPU, then GPU, then CPU. The device that actually executes must be logged.
    #[default]
    Auto,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_deterministic_layer_can_block() {
        assert_eq!(Layer::Deterministic.max_decision(), Decision::Blocked);
        assert_eq!(Layer::Ner.max_decision(), Decision::Masked);
        assert_eq!(Layer::Policy.max_decision(), Decision::Masked);
    }

    #[test]
    fn ner_findings_cannot_escalate_to_block() {
        let finding = Finding {
            span: 0..4,
            kind: DataKind::Person,
            confidence: 0.99,
            layer: Layer::Ner,
        };
        assert_eq!(finding.clamp_decision(Decision::Blocked), Decision::Masked);
    }

    #[test]
    fn deterministic_findings_keep_a_lesser_decision() {
        let finding = Finding {
            span: 0..11,
            kind: DataKind::Pesel,
            confidence: 1.0,
            layer: Layer::Deterministic,
        };
        assert_eq!(finding.clamp_decision(Decision::Advised), Decision::Advised);
    }
}
