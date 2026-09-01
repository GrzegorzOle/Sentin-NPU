// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Shared vocabulary for the Sentin-NPU inspection pipeline.
//!
//! This crate deliberately owns the types that encode the project's invariants, so that the
//! detection, proxy and audit crates cannot drift apart on them. In particular it encodes the
//! *advisory-first* rule: only the deterministic layer, on a checksum-valid match, may block.

#![warn(missing_docs)]

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
///
/// Each variant also accepts its imperative spelling when deserialised, because configuration
/// reads as an instruction (`mode: block`) while an audit event reads as a record of what
/// happened (`"decision": "blocked"`). One type serves both without forcing either to sound wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Logged only; the request passes through untouched.
    #[serde(alias = "observe")]
    Observed,
    /// The user was warned; the request passes through.
    #[serde(alias = "advise")]
    Advised,
    /// Sensitive spans were replaced before forwarding.
    #[serde(alias = "mask")]
    Masked,
    /// The request was refused. Reachable only from [`Layer::Deterministic`].
    #[serde(alias = "block")]
    Blocked,
}

/// The class of sensitive data a finding refers to.
///
/// Serialised into audit events as `data_type`. Never accompanied by the matched text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DataKind {
    /// Polish national identification number. Eleven digits carrying a birth date and a check
    /// digit, so a match is arithmetically verifiable.
    Pesel,
    /// Polish tax identification number (NIP), ten digits with a weighted check digit. Also
    /// recognised in its EU VAT form, `PL` followed by the same ten digits, which is how it is
    /// written on an invoice - the checksum is what makes both forms verifiable.
    Nip,
    /// A VAT identification number of another EU member state: two letters of country code and a
    /// national part whose length and shape that country prescribes.
    ///
    /// Shape only, so it never blocks. Each member state validates its own number differently -
    /// several with checksums this project does not implement - and asserting arithmetic proof we
    /// do not have is the false positive that gets a DLP tool switched off. A Polish number is not
    /// reported here: it has a checksum, so it is a [`DataKind::Nip`] with the evidence to match.
    VatEu,
    /// Polish business registry number (REGON), nine or fourteen digits, each length with its own
    /// check digit.
    Regon,
    /// International bank account number, validated by the mod-97 rule over the reordered string.
    Iban,
    /// Payment card number: a known issuer prefix *and* a valid Luhn check digit. Luhn alone
    /// accepts roughly one random digit string in ten, which is why the prefix is required too.
    PaymentCard,
    /// Email address. Shape only — there is no checksum in an address, so this never blocks.
    Email,
    /// Polish telephone number, recognised only with a `+48`/`0048` prefix or `123 456 789`
    /// grouping. A bare nine-digit run is left to REGON, since guessing "phone" from it would fire
    /// on every order id in every prompt.
    PhonePl,
    /// A person's name, from layer 2. Probabilistic, and the class most affected by Polish
    /// inflection.
    Person,
    /// An organisation or company name, from layer 2.
    Organization,
    /// A place name, from layer 2.
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
    /// Byte range into the original text. See the type's note on why bytes, not characters.
    pub span: std::ops::Range<usize>,
    /// What kind of sensitive data was found.
    pub kind: DataKind,
    /// 1.0 for checksum-validated deterministic matches; model score for NER.
    pub confidence: f32,
    /// Which detection layer produced this finding.
    pub layer: Layer,
    /// How well the finding is evidenced. Together with `layer` this bounds the verdict.
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
    /// Intel NPU through the OpenVINO NPU plugin. Available only where an Intel NPU and its kernel
    /// driver are present; no other vendor's NPU is an OpenVINO target.
    Npu,
    /// Whatever OpenVINO's GPU plugin binds through the OpenCL ICD loader — an Intel iGPU on the
    /// target hardware, but not necessarily elsewhere. On the dev machine it is an NVIDIA dGPU,
    /// roughly ten times slower than CPU for this model.
    Gpu,
    /// The OpenVINO CPU plugin. Always available, and the fallback everything else resolves to.
    Cpu,
    /// Try NPU, then GPU, then CPU. The device that actually executes must be logged.
    #[default]
    Auto,
}

impl Device {
    /// Parse a device name as OpenVINO reports it, case-insensitively.
    ///
    /// Needed because the inference engine hands back the executing device as a plain string while
    /// the audit schema types it. Returns `None` for anything unrecognised — an unknown device
    /// belongs in a free-form detail, not silently mapped onto one of these.
    ///
    /// OpenVINO reports multi-adapter systems as `GPU.0`, `GPU.1` and so on, so the match is on the
    /// prefix rather than the whole string.
    #[must_use]
    pub fn parse_name(name: &str) -> Option<Self> {
        let name = name.trim();
        for (prefix, device) in [
            ("NPU", Self::Npu),
            ("GPU", Self::Gpu),
            ("CPU", Self::Cpu),
            ("AUTO", Self::Auto),
        ] {
            if name.len() >= prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix) {
                return Some(device);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_parse_the_way_openvino_reports_them() {
        assert_eq!(Device::parse_name("NPU"), Some(Device::Npu));
        assert_eq!(Device::parse_name("cpu"), Some(Device::Cpu));
        // Multi-adapter machines report GPU.0, GPU.1 — still the GPU plugin.
        assert_eq!(Device::parse_name("GPU.1"), Some(Device::Gpu));
        // An unknown device must not be coerced into one of ours; the caller keeps it as detail.
        assert_eq!(Device::parse_name("VPUX"), None);
        assert_eq!(Device::parse_name(""), None);
    }

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
        for kind in [DataKind::Email, DataKind::PhonePl, DataKind::VatEu] {
            let f = finding(kind, Layer::Deterministic, Validation::Pattern);
            assert_eq!(
                f.clamp_decision(Decision::Blocked),
                Decision::Masked,
                "{kind:?} must not be blockable"
            );
        }
    }
}
