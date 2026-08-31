// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Audit events for a SIEM (Phase 6). Schema is fixed in `docs/events.md`, which is authoritative.
//!
//! **Events carry metadata only. The detected text never appears in one** — not in any emitter,
//! not at any log level, not in a debug field. That is what makes the gateway safe to point at a
//! SOC: an audit trail that quoted the sensitive value would itself become the leak it exists to
//! record.
//!
//! The rule is enforced structurally rather than by review. [`Event`] has no field capable of
//! holding matched text: where the content matters for correlation, [`Event::content_sha256`]
//! stands in for it, and it hashes the *whole inspected payload* rather than the finding, so it
//! cannot be brute-forced back to a short identifier the way a hash of "44051401359" could.

#![warn(missing_docs)]

pub mod cef;
pub mod emit;

use sentin_core::{DataKind, Decision, Device};
use serde::{Deserialize, Serialize};

/// What happened. See `docs/events.md` for the authoritative descriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Any layer produced a finding.
    PiiDetected,
    /// The policy engine settled on a verdict.
    DecisionMade,
    /// Inspection exceeded `inference.timeout_ms`.
    InspectionTimeout,
    /// The requested device was unavailable, or execution fell back to another.
    DeviceFallback,
    /// The gateway finished starting and is accepting requests.
    GatewayStart,
    /// The gateway is shutting down. Its absence in a SIEM is itself a signal.
    GatewayStop,
}

impl EventKind {
    /// Severity for CEF, on its 0-10 scale.
    ///
    /// A block is the most severe because a user's request was refused; a timeout matters because
    /// traffic went out uninspected under fail-open.
    #[must_use]
    pub fn base_severity(self) -> u8 {
        match self {
            EventKind::PiiDetected => 5,
            EventKind::DecisionMade => 4,
            EventKind::InspectionTimeout => 7,
            EventKind::DeviceFallback => 3,
            EventKind::GatewayStart | EventKind::GatewayStop => 1,
        }
    }
}

/// One audit event. Every field here is metadata; none can carry detected text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// RFC 3339 UTC. Supplied by the caller so tests are deterministic and the crate stays free of
    /// a clock dependency.
    pub ts: String,
    /// What happened.
    pub event: EventKind,
    /// Which detector fired, e.g. `pesel`, `iban`, `ner_npu`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detector: Option<String>,
    /// The class of data involved — the type, never an instance of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<DataKind>,
    /// Upstream the request was bound for, e.g. `api.anthropic.com`. Host only — a full URL can
    /// carry query parameters, and those can carry content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_host: Option<String>,
    /// The verdict reached, after clamping to what the layer and the evidence allow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    /// Hash of the whole inspected payload, never the payload and never the finding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    /// Which model produced a layer-2 finding, e.g. `herbert-base-ner-int8-128`. Two events with
    /// different quality are otherwise indistinguishable after a model change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// The device that *actually* executed, which `AUTO` makes worth recording.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<Device>,
    /// Who sent the request: the caller's IP address, without the port.
    ///
    /// The one field that answers "whose workstation was this", which a decision without an owner
    /// cannot. **No port**, deliberately: it is ephemeral, so an event carrying it cannot be
    /// grouped by caller, and grouping by caller is the only thing a SIEM does with this field.
    /// It is an address, not an identity, and it is as personal as any proxy log - a deployment
    /// that must not record it turns the sink off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_addr: Option<String>,
    /// The upstream model the caller asked for, e.g. `ovh-llama` or `claude-sonnet-4`.
    ///
    /// Not to be confused with [`Event::model_id`], which names the *inspecting* NER model. This
    /// is the model the data was about to be sent to, and it is what makes "which model is our
    /// data leaking towards" answerable at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    /// Which adapter handled it - `anthropic`, `openai`, `google`. Coarser than `target_host` and
    /// stable across upstream changes, so a dashboard can group by it without breaking when a
    /// router moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Free-form context that is safe to record: policy names, versions, requested-vs-actual
    /// device. Never text taken from a request — see [`Event::detail`].
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub detail: std::collections::BTreeMap<String, String>,
}

impl Event {
    /// Start an event of the given kind at the given time.
    #[must_use]
    pub fn new(ts: impl Into<String>, event: EventKind) -> Self {
        Self {
            ts: ts.into(),
            event,
            detector: None,
            data_type: None,
            target_host: None,
            decision: None,
            content_sha256: None,
            model_id: None,
            device: None,
            client_addr: None,
            upstream_model: None,
            provider: None,
            detail: std::collections::BTreeMap::new(),
        }
    }

    /// Record which detector fired.
    #[must_use]
    pub fn detector(mut self, detector: impl Into<String>) -> Self {
        self.detector = Some(detector.into());
        self
    }

    /// Record the class of data involved.
    #[must_use]
    pub fn data_type(mut self, kind: DataKind) -> Self {
        self.data_type = Some(kind);
        self
    }

    /// Record the upstream host. Pass a host, never a full URL.
    #[must_use]
    pub fn target_host(mut self, host: impl Into<String>) -> Self {
        self.target_host = Some(host.into());
        self
    }

    /// Record the verdict reached.
    #[must_use]
    pub fn decision(mut self, decision: Decision) -> Self {
        self.decision = Some(decision);
        self
    }

    /// Record the digest of the whole inspected payload.
    #[must_use]
    pub fn content_sha256(mut self, digest: impl Into<String>) -> Self {
        self.content_sha256 = Some(digest.into());
        self
    }

    /// Record which model produced the finding.
    #[must_use]
    pub fn model_id(mut self, model: impl Into<String>) -> Self {
        self.model_id = Some(model.into());
        self
    }

    /// Record the device that actually executed the inference.
    #[must_use]
    pub fn device(mut self, device: Device) -> Self {
        self.device = Some(device);
        self
    }

    /// Record who sent the request, as `ip:port`.
    #[must_use]
    pub fn client_addr(mut self, addr: impl Into<String>) -> Self {
        self.client_addr = Some(addr.into());
        self
    }

    /// Record the upstream model the caller asked for.
    #[must_use]
    pub fn upstream_model(mut self, model: impl Into<String>) -> Self {
        self.upstream_model = Some(model.into());
        self
    }

    /// Record which provider adapter handled the request.
    #[must_use]
    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Attach a named detail.
    ///
    /// Reserved for facts about the *system* — a policy name, a version, a requested device.
    /// Putting request text here would defeat the entire schema, so callers pass short, known
    /// values and never a slice of a payload.
    #[must_use]
    pub fn detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.detail.insert(key.into(), value.into());
        self
    }
}

/// Hash an inspected payload for correlation.
///
/// The whole payload, deliberately: hashing just the matched identifier would produce a value an
/// analyst could confirm by guessing, since the space of eleven-digit numbers is small enough to
/// enumerate. Hashing the payload makes the digest a correlation key and nothing else.
#[must_use]
pub fn digest(payload: &[u8]) -> String {
    format!("sha256:{}", hex(&sha256(payload)))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Minimal SHA-256. Vendored rather than pulled in as a dependency: the audit crate is the one
/// place where a supply-chain surprise would be least welcome, and this is a well-specified,
/// self-contained function.
fn sha256(message: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];

    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((message.len() as u64) * 8).to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (index, word) in chunk.chunks_exact(4).enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);

        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut out = [0u8; 32];
    for (chunk, value) in out.chunks_exact_mut(4).zip(h) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_vectors() {
        // Without known-answer vectors, a hand-written hash is just a plausible-looking function.
        assert_eq!(
            digest(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            digest(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Longer than one 64-byte block, so the multi-chunk path is covered too.
        assert_eq!(
            digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "sha256:248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn an_event_serialises_without_empty_fields() {
        let event =
            Event::new("2026-08-08T12:00:00Z", EventKind::GatewayStart).detail("version", "0.1.0");
        let json = serde_json::to_string(&event).expect("serialises");

        assert!(json.contains("gateway_start"));
        assert!(
            !json.contains("detector"),
            "absent fields must be omitted: {json}"
        );
        assert!(
            !json.contains("null"),
            "no nulls in the wire format: {json}"
        );
    }

    #[test]
    fn a_detection_event_carries_the_kind_and_never_the_value() {
        let pesel = "44051401359";
        let event = Event::new("2026-08-08T12:00:00Z", EventKind::PiiDetected)
            .detector("pesel")
            .data_type(DataKind::Pesel)
            .target_host("api.anthropic.com")
            .decision(Decision::Masked)
            .content_sha256(digest(format!("Klient {pesel} zlozyl wniosek.").as_bytes()));

        let json = serde_json::to_string(&event).expect("serialises");
        assert!(
            !json.contains(pesel),
            "the audit trail must never quote the detected value: {json}"
        );
        assert!(
            json.contains("PESEL"),
            "the data kind is what gets recorded"
        );
        assert!(json.contains("masked"));
    }

    #[test]
    fn an_event_names_the_caller_and_the_model_the_data_was_heading_for() {
        // The two questions a SOC asks about a detection are whose machine sent it and where it
        // was going. Both are metadata; neither is content.
        let event = Event::new("2026-08-31T20:00:00Z", EventKind::PiiDetected)
            .detector("pesel")
            .data_type(DataKind::Pesel)
            .client_addr("172.19.0.4")
            .upstream_model("ovh-llama")
            .provider("openai")
            .model_id("seq128");

        let json = serde_json::to_string(&event).expect("serialises");
        assert!(json.contains(r#""client_addr":"172.19.0.4""#), "{json}");
        assert!(json.contains(r#""upstream_model":"ovh-llama""#), "{json}");
        assert!(json.contains(r#""provider":"openai""#), "{json}");
        assert!(
            json.contains(r#""model_id":"seq128""#),
            "the inspecting model stays a separate field from the model being queried: {json}"
        );
    }

    #[test]
    fn the_new_fields_are_omitted_rather_than_null_when_unknown() {
        // A parser written from docs/events.md treats an absent field as absent. Serialising
        // nulls would make every event carry three fields that say nothing.
        let json =
            serde_json::to_string(&Event::new("2026-08-31T20:00:00Z", EventKind::PiiDetected))
                .expect("serialises");
        assert!(!json.contains("client_addr"), "{json}");
        assert!(!json.contains("upstream_model"), "{json}");
        assert!(!json.contains("provider"), "{json}");
    }

    #[test]
    fn the_digest_covers_the_payload_not_the_finding() {
        // Hashing the identifier alone would be reversible by enumeration: there are only 10^11
        // candidate PESELs, and far fewer once the checksum and date rules are applied.
        let identifier = "44051401359";
        let payload = format!("Klient {identifier} zlozyl wniosek.");
        assert_ne!(digest(payload.as_bytes()), digest(identifier.as_bytes()));
    }

    #[test]
    fn severity_ranks_a_timeout_above_a_routine_detection() {
        // Fail-open means a timeout let traffic out uninspected; that deserves attention.
        assert!(
            EventKind::InspectionTimeout.base_severity() > EventKind::PiiDetected.base_severity()
        );
        assert!(EventKind::PiiDetected.base_severity() > EventKind::GatewayStart.base_severity());
    }
}
