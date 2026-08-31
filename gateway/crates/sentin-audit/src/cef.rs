// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! CEF (Common Event Format) rendering and syslog delivery.
//!
//! CEF is what most SIEMs ingest without a custom parser, which is the whole point of emitting it:
//! a Polish SME under NIS2 should be able to point Wazuh or Elastic at this and see events, not
//! write a mapping first.
//!
//! Escaping is the fiddly part and it is a correctness issue, not cosmetics. A pipe or a backslash
//! passing through unescaped shifts every following field by one, so an event about a masked PESEL
//! could be ingested as an event about something else entirely.

use std::net::{ToSocketAddrs, UdpSocket};

use crate::emit::Emitter;
use crate::Event;

const VENDOR: &str = "Sentin";
const PRODUCT: &str = "Sentin-NPU";

/// Render an event as a CEF line.
///
/// `CEF:0|vendor|product|version|signature|name|severity|extensions`
#[must_use]
pub fn render(event: &Event, product_version: &str) -> String {
    let signature = match event.event {
        crate::EventKind::PiiDetected => "pii_detected",
        crate::EventKind::DecisionMade => "decision_made",
        crate::EventKind::InspectionTimeout => "inspection_timeout",
        crate::EventKind::DeviceFallback => "device_fallback",
        crate::EventKind::GatewayStart => "gateway_start",
        crate::EventKind::GatewayStop => "gateway_stop",
    };

    let mut extensions = Vec::new();
    let mut push =
        |key: &str, value: &str| extensions.push(format!("{key}={}", escape_value(value)));

    push("rt", &event.ts);
    if let Some(detector) = &event.detector {
        push("cs1Label", "detector");
        push("cs1", detector);
    }
    if let Some(kind) = event.data_type {
        push("cs2Label", "dataType");
        push("cs2", &format!("{kind:?}").to_uppercase());
    }
    if let Some(host) = &event.target_host {
        push("dhost", host);
    }
    if let Some(decision) = event.decision {
        push("act", &format!("{decision:?}").to_lowercase());
    }
    if let Some(hash) = &event.content_sha256 {
        push("fileHash", hash);
    }
    if let Some(model) = &event.model_id {
        push("cs3Label", "modelId");
        push("cs3", model);
    }
    if let Some(device) = event.device {
        push("cs4Label", "device");
        push("cs4", &format!("{device:?}").to_uppercase());
    }
    // The requester goes in CEF's own source fields rather than a custom string, so a SIEM that
    // knows CEF correlates it with everything else it already calls a source address.
    if let Some(addr) = &event.client_addr {
        match addr.rsplit_once(':') {
            Some((host, port)) => {
                push("src", host);
                push("spt", port);
            }
            None => push("src", addr),
        }
    }
    if let Some(model) = &event.upstream_model {
        push("cs5Label", "upstreamModel");
        push("cs5", model);
    }
    if let Some(provider) = &event.provider {
        push("cs6Label", "provider");
        push("cs6", provider);
    }
    for (key, value) in &event.detail {
        push(key, value);
    }

    format!(
        "CEF:0|{}|{}|{}|{}|{}|{}|{}",
        escape_header(VENDOR),
        escape_header(PRODUCT),
        escape_header(product_version),
        escape_header(signature),
        escape_header(signature),
        event.event.base_severity(),
        extensions.join(" ")
    )
}

/// Header fields escape `\` and `|`.
fn escape_header(value: &str) -> String {
    value.replace('\\', r"\\").replace('|', r"\|")
}

/// Extension values escape `\`, `=` and newlines. A raw newline would split one event into two.
fn escape_value(value: &str) -> String {
    value
        .replace('\\', r"\\")
        .replace('=', r"\=")
        .replace('\n', r"\n")
        .replace('\r', "")
}

/// Send CEF lines to a syslog collector over UDP.
///
/// UDP because that is what SIEM collectors most commonly expose and because it cannot block the
/// request path waiting for a peer. Delivery is therefore best-effort by design, which is the
/// right trade for a gateway that must not stall on its audit sink.
#[derive(Debug)]
pub struct SyslogEmitter {
    socket: UdpSocket,
    target: std::net::SocketAddr,
    product_version: String,
    facility_severity: u8,
}

impl SyslogEmitter {
    /// Bind a local socket and resolve the collector address.
    ///
    /// # Errors
    /// Fails if the address cannot be resolved or a local socket cannot be bound.
    pub fn udp(address: &str, product_version: impl Into<String>) -> std::io::Result<Self> {
        let target = address
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| std::io::Error::other(format!("cannot resolve {address}")))?;
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        Ok(Self {
            socket,
            target,
            product_version: product_version.into(),
            // local0.notice — a conventional default for application audit streams.
            facility_severity: 133,
        })
    }
}

impl Emitter for SyslogEmitter {
    fn emit(&self, event: &Event) {
        let line = format!(
            "<{}>{} {}",
            self.facility_severity,
            event.ts,
            render(event, &self.product_version)
        );
        if let Err(err) = self.socket.send_to(line.as_bytes(), self.target) {
            tracing::warn!(target = %self.target, error = %err, "audit: syslog send failed");
        }
    }

    fn name(&self) -> &'static str {
        "cef-syslog"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{digest, EventKind};
    use sentin_core::{DataKind, Decision, Device};

    fn detection() -> Event {
        Event::new("2026-08-08T12:00:00Z", EventKind::PiiDetected)
            .detector("pesel")
            .data_type(DataKind::Pesel)
            .target_host("api.anthropic.com")
            .decision(Decision::Masked)
            .content_sha256(digest(b"Klient 44051401359 zlozyl wniosek."))
            .device(Device::Cpu)
    }

    /// Split a CEF line on **unescaped** pipes, the way a conforming parser does.
    ///
    /// Splitting on every pipe is what a naive reader does, and it is exactly the failure mode the
    /// escaping exists to prevent — so the test has to model the correct behaviour, not the naive
    /// one, or it asserts the wrong thing.
    fn cef_fields(line: &str) -> Vec<String> {
        let mut fields = vec![String::new()];
        let mut escaped = false;
        for ch in line.chars() {
            match ch {
                _ if escaped => {
                    fields.last_mut().expect("non-empty").push(ch);
                    escaped = false;
                }
                '\\' => escaped = true,
                '|' => fields.push(String::new()),
                _ => fields.last_mut().expect("non-empty").push(ch),
            }
        }
        fields
    }

    #[test]
    fn a_cef_line_has_the_seven_header_fields() {
        let line = render(&detection(), "0.1.0");
        assert!(line.starts_with("CEF:0|Sentin|Sentin-NPU|0.1.0|pii_detected|"));
        let fields = cef_fields(&line);
        assert_eq!(fields[0], "CEF:0");
        assert_eq!(fields[3], "0.1.0");
        assert_eq!(fields[4], "pii_detected");
        assert_eq!(fields[6], "5", "severity for a detection");
    }

    #[test]
    fn the_cef_line_never_contains_the_detected_value() {
        let line = render(&detection(), "0.1.0");
        assert!(!line.contains("44051401359"), "{line}");
        assert!(line.contains("cs2=PESEL"), "the kind is recorded: {line}");
        assert!(line.contains("act=masked"));
    }

    #[test]
    fn equals_and_backslashes_in_values_are_escaped() {
        // Unescaped, these shift every following field and the event is ingested as something else.
        let event = Event::new("2026-08-08T12:00:00Z", EventKind::DeviceFallback)
            .detail("requested", "a=b")
            .detail("note", r"back\slash");
        let line = render(&event, "0.1.0");
        assert!(line.contains(r"requested=a\=b"), "{line}");
        assert!(line.contains(r"note=back\\slash"), "{line}");
    }

    #[test]
    fn newlines_in_values_cannot_split_an_event() {
        let event =
            Event::new("2026-08-08T12:00:00Z", EventKind::GatewayStart).detail("note", "a\nb");
        let line = render(&event, "0.1.0");
        assert_eq!(
            line.lines().count(),
            1,
            "one event must be one line: {line}"
        );
    }

    #[test]
    fn pipes_in_the_version_do_not_break_the_header() {
        // A pipe smuggled into a header field would otherwise shift every later field by one, so
        // a conforming parser must still see the signature where it belongs.
        let line = render(&detection(), "0.1.0|evil");
        assert!(
            line.contains(r"0.1.0\|evil"),
            "the pipe must be escaped: {line}"
        );

        let fields = cef_fields(&line);
        assert_eq!(
            fields[3], "0.1.0|evil",
            "the version keeps its literal value"
        );
        assert_eq!(
            fields[4], "pii_detected",
            "signature stays in its own field"
        );
    }

    #[test]
    fn a_syslog_emitter_with_an_unreachable_target_does_not_panic() {
        // Nothing listens on this port; send_to on UDP still succeeds or fails quietly, and either
        // way the gateway must carry on.
        let emitter = SyslogEmitter::udp("127.0.0.1:1", "0.1.0").expect("binds");
        emitter.emit(&detection());
    }
}
