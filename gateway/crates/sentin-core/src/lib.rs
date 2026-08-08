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

/// How strongly a finding is evidenced.
///
/// The project's invariant is "only L1, **on a checksum-valid match**, may block". `Layer` alone
/// cannot express that: email and Polish phone numbers are found by layer 1 but have no checksum
/// to verify, so a plausible-looking string is all the evidence there is. Blocking a request on
/// that is exactly the false positive that makes a DLP tool get switched off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Validation {
    /// Verified arithmetically: PESEL, NIP, REGON, IBAN mod-97, card Luhn.
    Checksum,
    /// Shape only, no arithmetic proof. Advisory or masking at most, never blocking.
    Pattern,
}

impl Validation {
    /// The strongest decision this level of evidence may produce.
    #[must_use]
    pub fn max_decision(self) -> Decision {
        match self {
            Validation::Checksum => Decision::Blocked,
            Validation::Pattern => Decision::Masked,
        }
    }
}

/// A single detection: where it is, what it is, and how well it is evidenced.
///
/// `span` is a **byte** range into the *original* text — the natural unit for Rust string slicing,
/// and safe because every layer-1 pattern is ASCII. Layer 2 must convert the tokenizer's character
/// offsets to byte offsets before constructing a finding, or spans will be wrong on any text
/// containing non-ASCII characters — which, for Polish, is most of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub span: std::ops::Range<usize>,
    pub kind: DataKind,
    /// 1.0 for checksum-validated deterministic matches; model score for NER.
    pub confidence: f32,
    pub layer: Layer,
    pub validation: Validation,
}

impl Finding {
    /// The strongest decision this finding may justify: the stricter of layer and evidence.
    #[must_use]
    pub fn max_decision(&self) -> Decision {
        self.layer
            .max_decision()
            .min(self.validation.max_decision())
    }

    /// Clamp a proposed decision to what this finding may justify.
    #[must_use]
    pub fn clamp_decision(&self, proposed: Decision) -> Decision {
        proposed.min(self.max_decision())
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

    fn finding(kind: DataKind, layer: Layer, validation: Validation) -> Finding {
        Finding {
            span: 0..11,
            kind,
            confidence: 1.0,
            layer,
            validation,
        }
    }

    #[test]
    fn ner_findings_cannot_escalate_to_block() {
        let f = finding(DataKind::Person, Layer::Ner, Validation::Pattern);
        assert_eq!(f.clamp_decision(Decision::Blocked), Decision::Masked);
    }

    #[test]
    fn deterministic_findings_keep_a_lesser_decision() {
        let f = finding(DataKind::Pesel, Layer::Deterministic, Validation::Checksum);
        assert_eq!(f.clamp_decision(Decision::Advised), Decision::Advised);
    }

    #[test]
    fn checksum_backed_layer_one_findings_may_block() {
        let f = finding(DataKind::Pesel, Layer::Deterministic, Validation::Checksum);
        assert_eq!(f.clamp_decision(Decision::Blocked), Decision::Blocked);
    }

    #[test]
    fn pattern_only_findings_cannot_block_even_in_layer_one() {
        // An email or phone number has no checksum: shape alone must never justify refusing a
        // request, however confidently the regex matched.
        for kind in [DataKind::Email, DataKind::PhonePl] {
            let f = finding(kind, Layer::Deterministic, Validation::Pattern);
            assert_eq!(
                f.clamp_decision(Decision::Blocked),
                Decision::Masked,
                "{kind:?} must not be blockable"
            );
        }
    }
}
