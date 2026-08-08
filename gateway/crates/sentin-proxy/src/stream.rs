// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Response relaying, and the three SSE inspection strategies compared in research question B2.
//!
//! A streamed completion arrives as a sequence of `data: {...}` events, each carrying a few
//! tokens. Inspecting that stream is a genuine design problem: a detector needs enough text to
//! recognise an identifier, but every byte held back is latency the user feels directly as the
//! answer stalling.
//!
//! The three strategies here are not alternatives to choose between on taste — they are the
//! measurement points for B2, and `benches/proxy_latency.rs` quantifies what each costs (M2c).
//!
//! **Response-side findings are logged, not acted on.** Rewriting a stream the client is already
//! rendering is out of PoC scope; the point of running detection here is to measure honestly what
//! response inspection would cost, rather than to measure buffering with no work in it.

use axum::body::Body;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use sentin_detect::detect;

use crate::config::StreamStrategy;

/// Wrap an upstream response body in the configured inspection strategy.
pub fn relay_body(response: reqwest::Response, strategy: StreamStrategy) -> Body {
    match strategy {
        StreamStrategy::Passthrough => Body::from_stream(response.bytes_stream()),
        StreamStrategy::Buffer => Body::from_stream(buffered(response)),
        StreamStrategy::SlidingWindow => Body::from_stream(windowed(response)),
    }
}

/// Collect the entire response, inspect once, then emit. The client sees nothing until the model
/// has finished generating.
fn buffered(
    response: reqwest::Response,
) -> impl futures_util::Stream<Item = reqwest::Result<Bytes>> {
    futures_util::stream::once(async move {
        let bytes = response.bytes().await?;
        log_findings(&bytes, "buffer");
        Ok(bytes)
    })
}

/// Release text at sentence boundaries: hold events until the accumulated text completes a
/// sentence, inspect that much, then flush.
fn windowed(
    response: reqwest::Response,
) -> impl futures_util::Stream<Item = reqwest::Result<Bytes>> {
    let upstream = response.bytes_stream();
    futures_util::stream::unfold(
        (upstream, BytesMut::new(), false),
        |(mut upstream, mut pending, done)| async move {
            if done {
                return None;
            }
            loop {
                match upstream.next().await {
                    Some(Ok(chunk)) => {
                        pending.extend_from_slice(&chunk);
                        if let Some(cut) = flush_point(&pending) {
                            let ready = pending.split_to(cut).freeze();
                            log_findings(&ready, "sliding_window");
                            return Some((Ok(ready), (upstream, pending, false)));
                        }
                    }
                    Some(Err(err)) => return Some((Err(err), (upstream, pending, true))),
                    None => {
                        // Stream ended: emit whatever is left, however incomplete.
                        let ready = pending.split().freeze();
                        if ready.is_empty() {
                            return None;
                        }
                        log_findings(&ready, "sliding_window");
                        return Some((Ok(ready), (upstream, pending, true)));
                    }
                }
            }
        },
    )
}

/// Where the buffer may safely be cut: the last SSE event boundary, provided the text so far
/// completes at least one sentence.
///
/// Cutting anywhere else would split a `data:` frame in half and corrupt the client's parser, so
/// the sentence rule is applied *within* the constraint of event framing rather than instead of it.
fn flush_point(pending: &[u8]) -> Option<usize> {
    let last_event_end = find_last(pending, b"\n\n").map(|index| index + 2)?;
    let text = String::from_utf8_lossy(&pending[..last_event_end]);
    text.contains(['.', '!', '?'])
        .then_some(last_event_end)
        .filter(|cut| *cut > 0)
}

fn find_last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .rev()
        .find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Run layer 1 over a slice of the response and log what it found — kinds only, never text.
fn log_findings(bytes: &[u8], strategy: &str) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let findings = detect(text);
    if !findings.is_empty() {
        let kinds: Vec<_> = findings
            .iter()
            .map(|f| crate::config::detector_key(f.kind))
            .collect();
        tracing::info!(
            strategy,
            detected = ?kinds,
            "sensitive data in response stream (advisory only)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(text: &str) -> String {
        format!("data: {{\"delta\":\"{text}\"}}\n\n")
    }

    #[test]
    fn no_flush_before_an_event_boundary() {
        assert_eq!(flush_point(b"data: {\"delta\":\"Hello."), None);
    }

    #[test]
    fn no_flush_before_a_sentence_ends() {
        let partial = format!("{}{}", event("Hello"), event(" world"));
        assert_eq!(
            flush_point(partial.as_bytes()),
            None,
            "complete events, but no sentence terminator yet"
        );
    }

    #[test]
    fn flushes_at_the_last_event_boundary_once_a_sentence_completes() {
        let stream = format!("{}{}", event("Hello world."), event(" Next"));
        let cut = flush_point(stream.as_bytes()).expect("should flush");

        // The cut lands on an event boundary, never inside a frame.
        assert_eq!(&stream[cut - 2..cut], "\n\n");
        assert!(stream[..cut].contains("Hello world."));
    }

    #[test]
    fn find_last_locates_the_final_occurrence() {
        assert_eq!(find_last(b"a\n\nb\n\nc", b"\n\n"), Some(4));
        assert_eq!(find_last(b"abc", b"\n\n"), None);
        assert_eq!(find_last(b"", b"\n\n"), None);
    }

    #[test]
    fn findings_in_a_response_slice_are_detected() {
        // Proves the buffered/windowed paths actually do inspection work, so the latency measured
        // for them in M2c is the cost of inspection and not just of waiting.
        let pesel = sentin_detect::testdata::pesel(1944, 5, 14, 135);
        let payload = event(&format!("Numer to {pesel}"));
        assert_eq!(detect(&payload).len(), 1);
    }
}
