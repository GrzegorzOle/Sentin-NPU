// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! OTLP export over HTTP with JSON encoding.
//!
//! The specification allows `application/json` alongside protobuf, and taking that route avoids
//! pulling protobuf codegen and a gRPC stack into a gateway whose whole appeal is being small.
//! Collectors accept it on `/v1/logs`.
//!
//! Events are queued and sent from a background task. Emitting must never block the request path:
//! an audit sink is not worth adding latency to a user's prompt, and it is certainly not worth
//! failing one.

use sentin_audit::emit::Emitter;
use sentin_audit::Event;
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

/// Sends events to an OTLP/HTTP collector.
#[derive(Debug)]
pub struct OtlpEmitter {
    sender: UnboundedSender<Event>,
}

impl OtlpEmitter {
    /// Start the exporter.
    ///
    /// # Errors
    /// Fails when no tokio runtime is available, since the sender needs somewhere to run.
    pub fn new(endpoint: &str, version: &str) -> Result<Self, String> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "OTLP export needs a tokio runtime".to_string())?;

        let url = if endpoint.ends_with("/v1/logs") {
            endpoint.to_string()
        } else {
            format!("{}/v1/logs", endpoint.trim_end_matches('/'))
        };
        let version = version.to_string();
        let (sender, mut receiver) = unbounded_channel::<Event>();

        handle.spawn(async move {
            let client = reqwest::Client::new();
            // Small batches: a SIEM prefers a steady trickle to occasional bursts, and a crash
            // then loses at most a handful of events rather than a whole window.
            let mut batch: Vec<Event> = Vec::with_capacity(32);
            loop {
                let Some(first) = receiver.recv().await else {
                    break;
                };
                batch.push(first);
                while let Ok(next) = receiver.try_recv() {
                    batch.push(next);
                    if batch.len() >= 32 {
                        break;
                    }
                }
                let payload = to_otlp(&batch, &version);
                if let Err(err) = client.post(&url).json(&payload).send().await {
                    tracing::warn!(%url, error = %err, "audit: OTLP export failed");
                }
                batch.clear();
            }
        });

        Ok(Self { sender })
    }
}

impl Emitter for OtlpEmitter {
    fn emit(&self, event: &Event) {
        // A closed channel means the exporter task is gone. Log once per event rather than
        // failing: the request this event describes has already been handled correctly.
        if self.sender.send(event.clone()).is_err() {
            tracing::warn!("audit: OTLP exporter stopped");
        }
    }

    fn name(&self) -> &'static str {
        "otlp"
    }
}

/// Wrap events in the OTLP logs envelope.
///
/// Each event becomes one log record whose attributes are the event's own fields, so a collector
/// can filter on `data_type` or `decision` without parsing a message string.
#[must_use]
pub fn to_otlp(events: &[Event], version: &str) -> Value {
    let records: Vec<Value> = events
        .iter()
        .map(|event| {
            let mut attributes = vec![attribute("event", &format!("{:?}", event.event))];
            let mut add = |key: &str, value: Option<String>| {
                if let Some(value) = value {
                    attributes.push(attribute(key, &value));
                }
            };
            add("detector", event.detector.clone());
            add("data_type", event.data_type.map(|k| format!("{k:?}")));
            add("target_host", event.target_host.clone());
            add("decision", event.decision.map(|d| format!("{d:?}")));
            add("content_sha256", event.content_sha256.clone());
            add("model_id", event.model_id.clone());
            add("device", event.device.map(|d| format!("{d:?}")));
            add("client_addr", event.client_addr.clone());
            add("upstream_model", event.upstream_model.clone());
            add("provider", event.provider.clone());
            for (key, value) in &event.detail {
                attributes.push(attribute(key, value));
            }

            json!({
                "timeUnixNano": "0",
                "observedTimeUnixNano": "0",
                "severityNumber": i32::from(event.event.base_severity()),
                "severityText": format!("{:?}", event.event),
                // The body names the event; every fact lives in attributes, and none of them can
                // carry request text.
                "body": {"stringValue": event.ts.clone()},
                "attributes": attributes,
            })
        })
        .collect();

    json!({
        "resourceLogs": [{
            "resource": {"attributes": [
                attribute("service.name", "sentin-npu"),
                attribute("service.version", version),
            ]},
            "scopeLogs": [{
                "scope": {"name": "sentin-audit"},
                "logRecords": records,
            }],
        }]
    })
}

fn attribute(key: &str, value: &str) -> Value {
    json!({"key": key, "value": {"stringValue": value}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentin_audit::{digest, EventKind};
    use sentin_core::{DataKind, Decision};

    fn detection() -> Event {
        Event::new("2026-08-08T12:00:00Z", EventKind::PiiDetected)
            .detector("pesel")
            .data_type(DataKind::Pesel)
            .target_host("api.anthropic.com")
            .decision(Decision::Masked)
            .content_sha256(digest(b"Klient 44051401359 zlozyl wniosek."))
    }

    #[test]
    fn the_envelope_has_the_shape_a_collector_expects() {
        let payload = to_otlp(&[detection()], "0.0.1");
        let logs = &payload["resourceLogs"][0]["scopeLogs"][0]["logRecords"];
        assert_eq!(logs.as_array().map(Vec::len), Some(1));
        assert_eq!(
            payload["resourceLogs"][0]["resource"]["attributes"][0]["value"]["stringValue"],
            "sentin-npu"
        );
    }

    #[test]
    fn facts_become_attributes_so_a_collector_can_filter_on_them() {
        let payload = to_otlp(&[detection()], "0.0.1");
        let text = payload.to_string();
        assert!(text.contains("\"key\":\"data_type\""), "{text}");
        assert!(text.contains("\"key\":\"decision\""));
        assert!(text.contains("\"key\":\"target_host\""));
    }

    #[test]
    fn the_otlp_payload_never_contains_the_detected_value() {
        let payload = to_otlp(&[detection()], "0.0.1").to_string();
        assert!(!payload.contains("44051401359"), "{payload}");
        assert!(payload.contains("Pesel"), "the kind is what travels");
    }

    #[test]
    fn a_batch_becomes_several_records_under_one_resource() {
        let payload = to_otlp(&[detection(), detection(), detection()], "0.0.1");
        assert_eq!(
            payload["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
        assert_eq!(payload["resourceLogs"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn constructing_without_a_runtime_fails_clearly_rather_than_panicking() {
        let result = OtlpEmitter::new("http://localhost:4318", "0.0.1");
        assert!(result.is_err(), "no tokio runtime in a plain unit test");
    }

    #[tokio::test]
    async fn an_unreachable_collector_does_not_block_emitting() {
        // Nothing listens there; emit must return immediately and the gateway carry on.
        let emitter = OtlpEmitter::new("http://127.0.0.1:1", "0.0.1").expect("constructs");
        emitter.emit(&detection());
        emitter.emit(&detection());
    }
}
