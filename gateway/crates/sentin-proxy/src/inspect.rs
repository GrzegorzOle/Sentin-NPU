// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! The inspection pipeline: extract text, detect, decide, and mask.
//!
//! Nothing in this module keeps the matched text. A [`FindingSummary`] carries the data *kind* and
//! the action taken, which is what an audit event is allowed to contain; the sensitive string
//! itself never leaves the buffer it arrived in.

use sentin_audit::Source;
use sentin_core::{DataKind, Decision};
use sentin_detect::detect;
use serde_json::Value;

use crate::adapters::{self, Provider};
use crate::config::{detector_key, Config};
use crate::ner_service::{NerService, Skipped};

/// What inspection concluded about one request. Metadata only, never content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingSummary {
    /// What kind of data was found. The class, never an instance of it.
    pub kind: DataKind,
    /// The action this finding justified, after clamping by evidence and configuration.
    pub decision: Decision,
    /// Where it was: the prompt, or a file attached to it. Two different incidents otherwise wear
    /// the same clothes - a typed identifier is one person's slip, a contract full of them is not.
    pub source: Source,
    /// What the attachment was and how big, when the finding came from one.
    pub attachment: Option<AttachmentInfo>,
}

/// What an attachment turned out to be, for the audit trail. Metadata only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentInfo {
    /// `pdf`, `ooxml`, `text` or `opaque`, decided from the bytes rather than the declared type.
    pub kind: String,
    /// Decoded size in bytes.
    pub bytes: u64,
    /// Digest of the decoded bytes, as `sha256:<hex>`. **This is how a file is named here**, since
    /// a filename would carry content into the audit trail. It equals `sha256sum` of the file on
    /// disk, and it survives a rename - which is the first thing somebody retrying a refused upload
    /// changes.
    pub sha256: String,
}

/// The result of inspecting one request: what was found, what to do, and what to forward.
#[derive(Debug, Clone)]
pub struct Inspection {
    /// The strongest action any finding justified.
    pub decision: Decision,
    /// One entry per finding, each with its own clamped verdict — a request can hold a blocking
    /// PESEL and an advisory name at once.
    pub findings: Vec<FindingSummary>,
    /// Present only when something was actually rewritten.
    pub masked_body: Option<Value>,
    /// Set when layer 2 did not contribute. The caller applies the timeout policy — inspection
    /// reports what happened and does not decide to refuse traffic on its own.
    pub ner_skipped: Option<Skipped>,
    /// Attachments that were present but could not be read: too large, encrypted, an image.
    ///
    /// Reported rather than ignored. An attachment nobody could inspect is precisely the one an
    /// operator may want to stop, and staying silent about it would claim a coverage the gateway
    /// does not have. Each entry is a short reason, **never a filename** - a filename carries
    /// content, and this string reaches the audit trail.
    pub unread_attachments: Vec<UnreadAttachment>,
}

/// An attachment that was present and could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadAttachment {
    /// Why, in the operator's words. **Never a filename** - a filename carries content.
    pub reason: String,
    /// What it appeared to be, when the bytes could be sniffed at all.
    pub kind: Option<String>,
    /// Decoded size in bytes, so an oversized export and an unreadable icon are countable apart.
    pub bytes: u64,
    /// Digest of the decoded bytes. `None` only when nothing decoded, which is the one case where
    /// there is no file to identify. An unreadable attachment is the one an operator most wants to
    /// follow, so it carries the same identifier as a readable one.
    pub sha256: Option<String>,
}

impl Inspection {
    /// The result for a request nothing was found in: forward it untouched, and emit no event.
    #[must_use]
    pub fn clean() -> Self {
        Self {
            decision: Decision::Observed,
            findings: Vec::new(),
            masked_body: None,
            ner_skipped: None,
            unread_attachments: Vec::new(),
        }
    }

    /// Compact, content-free description for logs and (in Phase 6) audit events.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.findings.is_empty() {
            return "clean".to_string();
        }
        self.findings
            .iter()
            .map(|f| match (&f.attachment, f.source) {
                // The suffix is what stops a log line reading as though someone typed a PESEL when
                // in fact they attached a document containing one.
                (Some(info), Source::Attachment) => {
                    format!("{}:{:?}@{}", detector_key(f.kind), f.decision, info.kind)
                }
                _ => format!("{}:{:?}", detector_key(f.kind), f.decision),
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Inspect a parsed request body against the configured policy.
///
/// Layer 1 runs inline; layer 2, when a service is supplied, runs on its own thread with a
/// timeout. A layer-2 timeout is reported through [`Inspection::ner_skipped`] rather than being
/// resolved here, because whether that should forward or refuse the request is operator policy.
pub async fn inspect_request(
    body: &Value,
    provider: Provider,
    config: &Config,
    ner: Option<&NerService>,
) -> Inspection {
    let pointers = provider.text_pointers(body);
    let mut findings = Vec::new();
    let mut overall = Decision::Observed;
    let mut rewrites: Vec<(String, String)> = Vec::new();
    let mut ner_skipped: Option<Skipped> = None;

    for pointer in pointers {
        let Some(text) = adapters::read_text(body, &pointer) else {
            continue;
        };

        let mut detected = detect(text);
        if let Some(service) = ner {
            match service.inspect(text).await {
                Ok(entities) => detected.extend(entities),
                // Record the first reason and carry on with layer 1: partial inspection is worth
                // more than none, and the caller still learns that layer 2 was incomplete.
                Err(reason) => {
                    ner_skipped.get_or_insert(reason);
                }
            }
        }
        if detected.is_empty() {
            continue;
        }

        let mut to_mask = Vec::new();
        for finding in &detected {
            // Two independent ceilings: what the operator asked for, and what the evidence
            // supports. Configuring `block` for a pattern-only detector cannot make it block.
            let decision = finding.clamp_decision(config.mode_for(finding.kind));
            findings.push(FindingSummary {
                kind: finding.kind,
                decision,
                source: Source::Prompt,
                attachment: None,
            });
            overall = overall.max(decision);
            if decision >= Decision::Masked {
                to_mask.push((finding.span.clone(), finding.kind));
            }
        }

        if !to_mask.is_empty() {
            rewrites.push((pointer, mask_spans(text, &to_mask)));
        }
    }

    // Attachments, after the prose. An identifier inside a document is exactly as much of a leak
    // as one in the prompt, and until this existed a PDF carrying a checksum-valid PESEL went
    // through as `findings=clean`.
    let mut unread_attachments = Vec::new();
    if config.inspect.attachments {
        let limits = sentin_extract::Limits {
            max_input_bytes: config.inspect.max_attachment_bytes,
            ..sentin_extract::Limits::default()
        };

        for attachment in provider.attachments(body) {
            let Some(payload) = adapters::read_text(body, &attachment.pointer) else {
                continue;
            };

            // Decoded first, then sniffed, so that even an attachment that cannot be read is
            // reported with what it was and how big: "an 8 MB PDF we could not open" and "a 2 KB
            // icon" are different events, and a dashboard cannot separate them from prose alone.
            let bytes = match sentin_extract::decode(payload) {
                Ok(bytes) => bytes,
                Err(err) => {
                    unread_attachments.push(UnreadAttachment {
                        reason: err.to_string(),
                        kind: None,
                        bytes: 0,
                        // Nothing decoded, so there is nothing whose digest would mean anything.
                        sha256: None,
                    });
                    continue;
                }
            };
            // Over the decoded bytes, so it equals `sha256sum` of the file the sender holds. This
            // is what identifies a document in the audit trail; a filename cannot, because a
            // filename carries content.
            let sha256 = sentin_audit::digest(&bytes);
            let sniffed = sentin_extract::sniff(&bytes).name().to_string();

            let extracted = match sentin_extract::extract(&bytes, &limits) {
                Ok(extracted) => extracted,
                Err(err) => {
                    unread_attachments.push(UnreadAttachment {
                        reason: err.to_string(),
                        kind: Some(sniffed),
                        bytes: bytes.len() as u64,
                        // The one an operator most wants to follow: an attachment nobody could
                        // read, arriving again from somewhere else.
                        sha256: Some(sha256),
                    });
                    continue;
                }
            };

            let mut detected = detect(&extracted.text);
            if let Some(service) = ner {
                match service.inspect(&extracted.text).await {
                    Ok(entities) => detected.extend(entities),
                    Err(reason) => {
                        ner_skipped.get_or_insert(reason);
                    }
                }
            }

            for finding in &detected {
                // The same two ceilings as prose, and then a third: **an attachment cannot be
                // masked**. Rewriting bytes inside a PDF or a zip would corrupt the document, so a
                // finding that would be masked in a prompt is only advised here. What survives is
                // blocking, which needs no rewrite - and which is the honest answer when a
                // checksum-valid national identifier is inside a file about to leave the machine.
                let decision = match finding.clamp_decision(config.mode_for(finding.kind)) {
                    Decision::Masked => Decision::Advised,
                    other => other,
                };
                findings.push(FindingSummary {
                    kind: finding.kind,
                    decision,
                    source: Source::Attachment,
                    attachment: Some(AttachmentInfo {
                        kind: extracted.kind.name().to_string(),
                        bytes: extracted.input_bytes as u64,
                        sha256: sha256.clone(),
                    }),
                });
                overall = overall.max(decision);
            }

            if extracted.truncated {
                unread_attachments.push(UnreadAttachment {
                    reason: "attachment truncated at the text limit; findings are a lower bound"
                        .to_string(),
                    kind: Some(extracted.kind.name().to_string()),
                    bytes: extracted.input_bytes as u64,
                    sha256: Some(sha256),
                });
            }
        }
    }

    // Blocking supersedes masking: the request is refused, so there is nothing to forward.
    let masked_body = if overall == Decision::Masked && !rewrites.is_empty() {
        let mut copy = body.clone();
        for (pointer, replacement) in rewrites {
            adapters::write_text(&mut copy, &pointer, replacement);
        }
        Some(copy)
    } else {
        None
    };

    Inspection {
        decision: overall,
        findings,
        masked_body,
        ner_skipped,
        unread_attachments,
    }
}

/// Replace each span with a placeholder naming the data kind.
///
/// Replacement runs back to front so that earlier spans keep the offsets the detector reported.
/// The placeholder names the kind rather than blanking the text, because the model still needs to
/// know that *a* national identifier stood there for the answer to make sense.
#[must_use]
pub fn mask_spans(text: &str, spans: &[(std::ops::Range<usize>, DataKind)]) -> String {
    let mut ordered: Vec<_> = spans.to_vec();
    ordered.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));

    let mut out = text.to_string();
    let mut last_start = usize::MAX;
    for (span, kind) in ordered {
        // Overlapping findings would corrupt the string; keep the first (rightmost) one.
        if span.end > last_start {
            continue;
        }
        last_start = span.start;
        out.replace_range(span, &placeholder(kind));
    }
    out
}

fn placeholder(kind: DataKind) -> String {
    format!("[{}]", detector_key(kind).to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentin_detect::testdata;
    use serde_json::json;

    fn config_with(mode: &str) -> Config {
        serde_yaml_ng::from_str(&format!(
            "detectors:\n  pesel: {{ mode: {mode} }}\n  email: {{ mode: {mode} }}\n"
        ))
        .expect("valid config")
    }

    /// A tiny text attachment, and the two digests that could be confused for each other.
    ///
    /// `ATTACHMENT_SHA` is `sha256sum` of the decoded bytes, which is the property that makes the
    /// field usable: an analyst holding a suspect file can compute it and search the SIEM.
    /// `BASE64_SHA` is the digest of the transported form, and is here only so a test fails if the
    /// two are ever swapped - it would look right in every log and match nothing anybody could
    /// reproduce.
    const ATTACHMENT_B64: &str =
        "S2xpZW50IFBFU0VMIDAyMjUwNTE0NDY1LCBrb250YWt0IGFubmEuemFyZW1ic2thQGV4YW1wbGUuY29tCg==";
    const ATTACHMENT_SHA: &str =
        "sha256:a0615e966838ad1da97490392f873cea9c1fca0e90e0a7fb34e07fe046eccdba";
    const BASE64_SHA: &str =
        "sha256:390e10df766972d5a06d299383352df11c4db2ebe9a1333919b252360b602b1a";

    fn body_with_attachment(b64: &str) -> serde_json::Value {
        json!({"messages": [{"role": "user", "content": [
            {"type": "file", "file": {"filename": "umowa.pdf",
             "file_data": format!("data:text/plain;base64,{b64}")}}
        ]}]})
    }

    #[tokio::test]
    async fn an_attachment_is_identified_by_the_digest_of_its_decoded_bytes() {
        let result = inspect_request(
            &body_with_attachment(ATTACHMENT_B64),
            Provider::OpenAi,
            &config_with("block"),
            None,
        )
        .await;

        let finding = result
            .findings
            .iter()
            .find(|f| f.kind == sentin_core::DataKind::Pesel)
            .expect("the PESEL inside the file was found");
        let info = finding.attachment.as_ref().expect("it came from a file");

        assert_eq!(
            info.sha256, ATTACHMENT_SHA,
            "the digest must equal sha256sum of the file the sender holds"
        );
        assert_ne!(
            info.sha256, BASE64_SHA,
            "hashing the transported form would match nothing an analyst can compute"
        );
    }

    #[tokio::test]
    async fn the_same_file_keeps_its_digest_across_requests() {
        // The whole point of the field: following one document across callers, channels and days.
        // The payload digest cannot do this - it changes with every surrounding word.
        let mut digests = Vec::new();
        for prompt in ["pierwsza próba", "druga próba, inny tekst"] {
            let mut body = body_with_attachment(ATTACHMENT_B64);
            body["messages"][0]["content"]
                .as_array_mut()
                .expect("content is a block array")
                .push(json!({"type": "text", "text": prompt}));
            let result =
                inspect_request(&body, Provider::OpenAi, &config_with("block"), None).await;
            let info = result
                .findings
                .iter()
                .find_map(|f| f.attachment.clone())
                .expect("the attachment was inspected");
            digests.push(info.sha256);
        }
        assert_eq!(digests[0], digests[1]);
    }

    #[tokio::test]
    async fn an_attachment_nobody_could_read_still_carries_its_digest() {
        // A NUL byte makes it a binary this gateway will not read. It is also the attachment an
        // operator most wants to follow, so it must be identifiable all the same.
        let opaque = "AAECA/8AAAA=";
        let result = inspect_request(
            &body_with_attachment(opaque),
            Provider::OpenAi,
            &config_with("block"),
            None,
        )
        .await;

        let skipped = result
            .unread_attachments
            .first()
            .expect("an unreadable attachment is reported, not ignored");
        assert!(
            skipped
                .sha256
                .as_deref()
                .is_some_and(|d| d.starts_with("sha256:")),
            "{skipped:?}"
        );
    }

    #[tokio::test]
    async fn masking_replaces_the_identifier_and_nothing_else() {
        let pesel = testdata::pesel(1944, 5, 14, 135);
        let body = json!({"messages": [{"role": "user", "content": format!("Mój PESEL to {pesel}, dziękuję.")}]});

        let result = inspect_request(&body, Provider::OpenAi, &config_with("mask"), None).await;

        assert_eq!(result.decision, Decision::Masked);
        let masked = result.masked_body.expect("body was rewritten");
        assert_eq!(
            masked["messages"][0]["content"],
            "Mój PESEL to [PESEL], dziękuję."
        );
    }

    #[tokio::test]
    async fn masking_is_correct_with_several_findings_in_one_field() {
        let first = testdata::pesel(1944, 5, 14, 135);
        let second = testdata::pesel(1985, 1, 1, 1234);
        let body = json!({"messages": [{"role": "user",
            "content": format!("A: {first} oraz B: {second} koniec")}]});

        let result = inspect_request(&body, Provider::OpenAi, &config_with("mask"), None).await;
        assert_eq!(
            result.masked_body.expect("rewritten")["messages"][0]["content"],
            "A: [PESEL] oraz B: [PESEL] koniec"
        );
    }

    #[tokio::test]
    async fn masking_preserves_multibyte_text_around_the_span() {
        let pesel = testdata::pesel(1944, 5, 14, 135);
        let body = json!({"messages": [{"role": "user",
            "content": format!("Zażółć gęślą jaźń {pesel} zażółć")}]});

        let result = inspect_request(&body, Provider::OpenAi, &config_with("mask"), None).await;
        assert_eq!(
            result.masked_body.expect("rewritten")["messages"][0]["content"],
            "Zażółć gęślą jaźń [PESEL] zażółć"
        );
    }

    #[tokio::test]
    async fn configured_block_is_honoured_for_checksum_findings() {
        let pesel = testdata::pesel(1944, 5, 14, 135);
        let body = json!({"messages": [{"role": "user", "content": pesel}]});

        let result = inspect_request(&body, Provider::OpenAi, &config_with("block"), None).await;
        assert_eq!(result.decision, Decision::Blocked);
        assert!(
            result.masked_body.is_none(),
            "a blocked request is not forwarded, so there is nothing to mask"
        );
    }

    #[tokio::test]
    async fn configured_block_cannot_block_a_pattern_only_finding() {
        let body = json!({"messages": [{"role": "user", "content": "pisz na jan@example.com"}]});

        let result = inspect_request(&body, Provider::OpenAi, &config_with("block"), None).await;

        // The operator asked for `block`; the evidence only supports masking.
        assert_eq!(result.decision, Decision::Masked);
        assert_eq!(
            result.masked_body.expect("rewritten")["messages"][0]["content"],
            "pisz na [EMAIL]"
        );
    }

    #[tokio::test]
    async fn observe_mode_forwards_the_body_unchanged() {
        let pesel = testdata::pesel(1944, 5, 14, 135);
        let body = json!({"messages": [{"role": "user", "content": pesel}]});

        let result = inspect_request(&body, Provider::OpenAi, &config_with("observe"), None).await;
        assert_eq!(result.decision, Decision::Observed);
        assert!(result.masked_body.is_none());
        assert_eq!(
            result.findings.len(),
            1,
            "still recorded, just not acted on"
        );
    }

    #[tokio::test]
    async fn summaries_never_contain_the_detected_text() {
        let pesel = testdata::pesel(1944, 5, 14, 135);
        let body = json!({"messages": [{"role": "user", "content": pesel.clone()}]});

        let result = inspect_request(&body, Provider::OpenAi, &config_with("mask"), None).await;
        let summary = result.summary();

        assert!(
            !summary.contains(&pesel),
            "summary leaked content: {summary}"
        );
        assert_eq!(summary, "pesel:Masked");
    }

    #[tokio::test]
    async fn clean_requests_are_not_copied() {
        let body = json!({"messages": [{"role": "user", "content": "zwykłe pytanie o pogodę"}]});
        let result = inspect_request(&body, Provider::OpenAi, &config_with("mask"), None).await;

        assert_eq!(result.decision, Decision::Observed);
        assert!(result.findings.is_empty());
        assert!(result.masked_body.is_none(), "no clone on the clean path");
    }
}
