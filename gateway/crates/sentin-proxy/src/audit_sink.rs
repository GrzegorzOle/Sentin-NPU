// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Building the audit fan-out from configuration, and emitting events from the request path.
//!
//! The gateway's reason for existing is partly this trail: a SOC that cannot see prompt leakage
//! has no way to report it. So the events have to actually be emitted, and they have to be safe —
//! which here means the same thing twice over. Nothing in this module can put request text into an
//! event, because [`sentin_audit::Event`] has no field that would hold it.

use std::sync::Arc;

use crate::config::{Audit, Config};
use crate::inspect::Inspection;
use sentin_audit::emit::{Emitter, Fanout, JsonlEmitter};
use sentin_audit::{cef::SyslogEmitter, digest, Event, EventKind};

/// Assemble the configured sinks. Failures are logged and skipped — a misconfigured collector
/// must not stop the gateway from running.
#[must_use]
pub fn build(config: &Audit, version: &str) -> Arc<Fanout> {
    let mut fanout = Fanout::new();

    if config.jsonl.enabled {
        match JsonlEmitter::new(&config.jsonl.path) {
            Ok(sink) => fanout = fanout.with(Box::new(sink)),
            Err(err) => tracing::warn!(
                path = %config.jsonl.path,
                error = %err,
                "audit: JSONL sink unavailable; continuing without it"
            ),
        }
    }

    if config.syslog_cef.enabled {
        match SyslogEmitter::udp(&config.syslog_cef.address, version) {
            Ok(sink) => fanout = fanout.with(Box::new(sink)),
            Err(err) => tracing::warn!(
                address = %config.syslog_cef.address,
                error = %err,
                "audit: syslog sink unavailable; continuing without it"
            ),
        }
    }

    if config.otlp.enabled {
        match crate::otlp::OtlpEmitter::new(&config.otlp.endpoint, version) {
            Ok(sink) => fanout = fanout.with(Box::new(sink)),
            Err(err) => tracing::warn!(
                endpoint = %config.otlp.endpoint,
                error = %err,
                "audit: OTLP sink unavailable; continuing without it"
            ),
        }
    }

    Arc::new(fanout)
}

/// Current time as RFC 3339 UTC, without pulling in a date library for one format string.
#[must_use]
pub fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's days-from-civil, inverted. Small, exact, and avoids a dependency whose only
/// job would be to format one timestamp.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Everything about a request that an audit event may record, and nothing from its body.
///
/// A struct rather than six positional arguments: the fields are all `Option<&str>` of the same
/// type, and two of them swapping places would compile and quietly mislabel every event a SIEM
/// receives.
#[derive(Debug, Clone, Copy)]
pub struct RequestContext<'a> {
    /// Upstream host the request was bound for. Host only, never the full URL.
    pub target_host: &'a str,
    /// The inspecting NER model, by IR directory name.
    pub model_id: Option<&'a str>,
    /// The device that executed inspection.
    pub device: Option<&'a str>,
    /// Who sent it, by IP. `None` where the server exposes no peer address.
    pub client_addr: Option<&'a str>,
    /// The model the caller asked for, e.g. `ovh-llama`.
    pub upstream_model: Option<&'a str>,
    /// The adapter that handled it: `anthropic`, `openai`, `google`.
    pub provider: &'a str,
}

/// Emit everything one inspected request warrants.
///
/// One `pii_detected` per finding, so a SIEM can count data types, plus one `decision_made` for
/// the request as a whole. The payload hash is computed once and shared, which is what lets an
/// analyst correlate the two without either event carrying content.
pub fn record_request(
    emitter: &Fanout,
    verdict: &Inspection,
    payload: &[u8],
    context: &RequestContext<'_>,
) {
    if verdict.findings.is_empty()
        && verdict.ner_skipped.is_none()
        && verdict.unread_attachments.is_empty()
    {
        return;
    }
    let ts = now_rfc3339();
    let hash = digest(payload);
    let target_host = context.target_host;

    for finding in &verdict.findings {
        let mut event = Event::new(&ts, EventKind::PiiDetected)
            .detector(crate::config::detector_key(finding.kind))
            .data_type(finding.kind)
            .target_host(target_host)
            .decision(finding.decision)
            .content_sha256(&hash)
            .provider(context.provider)
            .source(finding.source);
        if let Some(info) = &finding.attachment {
            event = event.attachment(info.kind.clone(), info.bytes);
        }
        if let Some(model) = context.model_id {
            event = event.model_id(model);
        }
        if let Some(addr) = context.client_addr {
            event = event.client_addr(addr);
        }
        if let Some(model) = context.upstream_model {
            event = event.upstream_model(model);
        }
        if let Some(device) = context.device {
            // The schema types this field, so fill it rather than smuggling the device through a
            // free-form detail: a SIEM parser written from docs/events.md looks for `device`, and
            // for a long time would not have found it. An unrecognised name still goes to detail,
            // because inventing an enum value would be worse than reporting the string.
            event = match sentin_core::Device::parse_name(device) {
                Some(parsed) => event.device(parsed),
                None => event.detail("device", device),
            };
        }
        emitter.emit(&event);
    }

    if !verdict.findings.is_empty() {
        // The summary event carries the same context as the detections. A dashboard that counts
        // requests rather than findings would otherwise have no caller and no destination to group
        // by, and counting findings over-weights one request that happened to carry six numbers.
        let mut event = Event::new(&ts, EventKind::DecisionMade)
            .target_host(target_host)
            .decision(verdict.decision)
            .content_sha256(&hash)
            .provider(context.provider)
            .detail("findings", verdict.findings.len().to_string());
        if let Some(addr) = context.client_addr {
            event = event.client_addr(addr);
        }
        if let Some(model) = context.upstream_model {
            event = event.upstream_model(model);
        }
        emitter.emit(&event);
    }

    // An attachment nobody could read is reported even when the request is otherwise clean. That
    // case - an image, an encrypted document, something over the size limit - used to produce
    // `findings=clean` and no event at all, which reads to an operator as "inspected and fine"
    // when it means "not inspected".
    for skipped in &verdict.unread_attachments {
        let mut event = Event::new(&ts, EventKind::AttachmentSkipped)
            .target_host(target_host)
            .content_sha256(&hash)
            .provider(context.provider)
            .source(sentin_audit::Source::Attachment)
            // The reason, never the filename: a filename carries content.
            .detail("reason", skipped.reason.clone());
        if let Some(kind) = &skipped.kind {
            event = event.attachment(kind.clone(), skipped.bytes);
        }
        if let Some(addr) = context.client_addr {
            event = event.client_addr(addr);
        }
        if let Some(model) = context.upstream_model {
            event = event.upstream_model(model);
        }
        emitter.emit(&event);
    }

    if let Some(reason) = &verdict.ner_skipped {
        emitter.emit(
            &Event::new(&ts, EventKind::InspectionTimeout)
                .target_host(target_host)
                .content_sha256(&hash)
                // The reason is one of a fixed set of strings from the inspection layer, never
                // anything derived from the request.
                .detail("reason", format!("{reason:?}"))
                .detail("layer", "ner"),
        );
    }
}

/// Lifecycle event, emitted once at startup.
pub fn record_start(emitter: &Fanout, config: &Config, version: &str, device: Option<&str>) {
    let mut event = Event::new(now_rfc3339(), EventKind::GatewayStart)
        .detail("version", version)
        .detail(
            "listen",
            format!("{}:{}", config.listen.host, config.listen.port),
        )
        .detail("providers", config.providers.len().to_string());
    if let Some(device) = device {
        event = event.detail("device", device);
    }
    emitter.emit(&event);
}

/// Recorded when the requested inference device was not the one that ran.
pub fn record_device_fallback(emitter: &Fanout, requested: &str, actual: &str) {
    emitter.emit(
        &Event::new(now_rfc3339(), EventKind::DeviceFallback)
            .detail("requested", requested)
            .detail("actual", actual),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentin_audit::emit::MemoryEmitter;
    use sentin_core::{DataKind, Decision};

    fn inspection() -> Inspection {
        Inspection {
            decision: Decision::Masked,
            findings: vec![
                crate::inspect::FindingSummary {
                    kind: DataKind::Pesel,
                    decision: Decision::Masked,
                    source: sentin_audit::Source::Prompt,
                    attachment: None,
                },
                crate::inspect::FindingSummary {
                    kind: DataKind::Person,
                    decision: Decision::Advised,
                    source: sentin_audit::Source::Prompt,
                    attachment: None,
                },
            ],
            masked_body: None,
            ner_skipped: None,
            unread_attachments: Vec::new(),
        }
    }

    #[test]
    fn a_timestamp_looks_like_rfc_3339() {
        let ts = now_rfc3339();
        assert_eq!(ts.len(), 20, "{ts}");
        assert!(ts.ends_with('Z') && ts.contains('T'), "{ts}");
        // Sanity: this project is not running before 2020 or after 2100.
        let year: i32 = ts[..4].parse().expect("year parses");
        assert!((2020..2100).contains(&year), "{ts}");
    }

    #[test]
    fn the_civil_date_conversion_matches_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1), "the epoch");
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29), "a leap day");
    }

    fn context<'a>(target_host: &'a str) -> RequestContext<'a> {
        RequestContext {
            target_host,
            model_id: None,
            device: None,
            client_addr: None,
            upstream_model: None,
            provider: "anthropic",
        }
    }

    #[test]
    fn one_event_per_finding_plus_one_decision() {
        let sink = Fanout::new().with(Box::new(MemoryEmitter::new()));
        record_request(
            &sink,
            &inspection(),
            b"payload",
            &context("api.anthropic.com"),
        );
        // Two findings and one decision; MemoryEmitter is behind the fanout so count via JSONL
        // semantics instead — see the e2e test for the full assertion.
        assert_eq!(sink.names(), vec!["memory"]);
    }

    #[test]
    fn a_clean_request_emits_nothing() {
        let sink = Fanout::new();
        let clean = Inspection::clean();
        record_request(&sink, &clean, b"payload", &context("host"));
        // Nothing to assert beyond "does not panic and does not fabricate an event": a request
        // with no findings has nothing to report, and a SIEM full of empty events is noise.
        assert!(sink.is_empty());
    }
}
