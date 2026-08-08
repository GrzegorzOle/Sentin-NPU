// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! The inspection pipeline: extract text, detect, decide, and mask.
//!
//! Nothing in this module keeps the matched text. A [`FindingSummary`] carries the data *kind* and
//! the action taken, which is what an audit event is allowed to contain; the sensitive string
//! itself never leaves the buffer it arrived in.

use sentin_core::{DataKind, Decision};
use sentin_detect::detect;
use serde_json::Value;

use crate::adapters::{self, Provider};
use crate::config::{detector_key, Config};
use crate::ner_service::{NerService, Skipped};

/// What inspection concluded about one request. Metadata only, never content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingSummary {
    pub kind: DataKind,
    /// The action this finding justified, after clamping by evidence and configuration.
    pub decision: Decision,
}

#[derive(Debug, Clone)]
pub struct Inspection {
    /// The strongest action any finding justified.
    pub decision: Decision,
    pub findings: Vec<FindingSummary>,
    /// Present only when something was actually rewritten.
    pub masked_body: Option<Value>,
    /// Set when layer 2 did not contribute. The caller applies the timeout policy — inspection
    /// reports what happened and does not decide to refuse traffic on its own.
    pub ner_skipped: Option<Skipped>,
}

impl Inspection {
    #[must_use]
    pub fn clean() -> Self {
        Self {
            decision: Decision::Observed,
            findings: Vec::new(),
            masked_body: None,
            ner_skipped: None,
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
            .map(|f| format!("{}:{:?}", detector_key(f.kind), f.decision))
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
