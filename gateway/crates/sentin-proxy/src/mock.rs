// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! A mock LLM provider, for end-to-end tests and latency measurement.
//!
//! Measuring proxy overhead against a real provider would measure the internet: a round trip to
//! `api.anthropic.com` varies by tens of milliseconds, which is an order of magnitude more than
//! the sub-millisecond overhead M2a is trying to resolve. A local mock makes the gateway's own
//! cost the only variable, and makes the whole measurement reproducible without an API key.
//!
//! The mock also **records the bodies it receives**, which is what lets an end-to-end test assert
//! that the masked text — and not the original — actually reached the upstream.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, post};
use axum::Router;
use serde_json::Value;

/// Bodies received by the mock, newest last.
#[derive(Clone, Debug, Default)]
pub struct Received(Arc<Mutex<Vec<Value>>>);

impl Received {
    #[must_use]
    pub fn all(&self) -> Vec<Value> {
        self.0.lock().expect("mock state not poisoned").clone()
    }

    #[must_use]
    pub fn last(&self) -> Option<Value> {
        self.all().last().cloned()
    }

    /// Every text field of every recorded body, flattened — enough to assert on what leaked.
    #[must_use]
    pub fn all_text(&self) -> String {
        self.all()
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// How many SSE events the mock emits, and how long it waits between them.
#[derive(Clone, Copy, Debug)]
pub struct StreamShape {
    pub events: usize,
    pub gap_ms: u64,
}

impl Default for StreamShape {
    fn default() -> Self {
        // Roughly a token every 12 ms over ~40 tokens: a short answer from a fast local model.
        Self {
            events: 40,
            gap_ms: 12,
        }
    }
}

#[derive(Clone, Debug)]
struct MockState {
    received: Received,
    shape: StreamShape,
}

/// Start the mock on an ephemeral port. Returns its base URL and the recorder.
///
/// # Panics
/// Panics if the loopback listener cannot be bound, which in a test means the environment is
/// broken rather than the code.
pub async fn spawn(shape: StreamShape) -> (String, Received) {
    let received = Received::default();
    let state = MockState {
        received: received.clone(),
        shape,
    };

    let app = Router::new()
        .route("/v1/messages", post(handle))
        .route("/v1/chat/completions", post(handle))
        .route("/v1beta/models/{model}", post(handle))
        .fallback(any(handle))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });

    (format!("http://{address}"), received)
}

async fn handle(State(state): State<MockState>, body: axum::body::Bytes) -> Response {
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let streaming = parsed
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    state
        .received
        .0
        .lock()
        .expect("mock state not poisoned")
        .push(parsed);

    if streaming {
        sse_response(state.shape)
    } else {
        axum::Json(serde_json::json!({
            "id": "msg_mock",
            "role": "assistant",
            "content": [{"type": "text", "text": "Potwierdzam otrzymanie wiadomości."}]
        }))
        .into_response()
    }
}

/// An SSE stream shaped like a token-by-token completion.
fn sse_response(shape: StreamShape) -> Response {
    let stream = futures_util::stream::unfold(0usize, move |index| async move {
        if index >= shape.events {
            return None;
        }
        if index > 0 && shape.gap_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(shape.gap_ms)).await;
        }
        // Sentences end every eighth event, which is what the sliding-window strategy keys on.
        let word = if index % 8 == 7 { "zdania." } else { "slowo" };
        let frame = format!("data: {{\"index\":{index},\"delta\":\"{word} \"}}\n\n");
        Some((
            Ok::<_, std::convert::Infallible>(bytes::Bytes::from(frame)),
            index + 1,
        ))
    });

    Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(axum::body::Body::from_stream(stream))
        .expect("static response builds")
}
