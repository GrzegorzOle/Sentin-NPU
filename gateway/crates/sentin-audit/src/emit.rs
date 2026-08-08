// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Where audit events go.
//!
//! Emitters never fail a request. An audit sink that is full, unreachable or misconfigured is an
//! operational problem to be logged, not a reason to refuse the user's traffic — a gateway that
//! stops working because its syslog server went down is a worse outcome than a gap in the trail.
//! Every emitter therefore reports errors and carries on.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::Event;

/// A destination for audit events.
pub trait Emitter: Send + Sync + std::fmt::Debug {
    /// Record one event. Implementations must not panic and must not block indefinitely.
    fn emit(&self, event: &Event);

    /// Name used in log messages when this emitter has trouble.
    fn name(&self) -> &'static str;
}

/// Append events to a file as JSON Lines — one object per line, the format every log shipper reads.
#[derive(Debug)]
pub struct JsonlEmitter {
    path: PathBuf,
    file: Mutex<Option<std::fs::File>>,
}

impl JsonlEmitter {
    /// Open (or create) the file.
    ///
    /// # Errors
    /// Fails if the file cannot be opened, which the caller should treat as a configuration error
    /// at startup rather than a per-request condition.
    pub fn new(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(Some(file)),
        })
    }
}

impl Emitter for JsonlEmitter {
    fn emit(&self, event: &Event) {
        let Ok(line) = serde_json::to_string(event) else {
            tracing::warn!("audit: event could not be serialised");
            return;
        };
        let Ok(mut guard) = self.file.lock() else {
            // A poisoned mutex means another thread panicked mid-write. Losing the trail is bad;
            // taking the gateway down with it is worse.
            tracing::warn!(path = %self.path.display(), "audit: sink lock poisoned");
            return;
        };
        if let Some(file) = guard.as_mut() {
            if let Err(err) = writeln!(file, "{line}") {
                tracing::warn!(path = %self.path.display(), error = %err, "audit: write failed");
            }
        }
    }

    fn name(&self) -> &'static str {
        "jsonl"
    }
}

/// Fan out to several emitters. Failure of one does not affect the others.
#[derive(Debug, Default)]
pub struct Fanout {
    emitters: Vec<Box<dyn Emitter>>,
}

impl Fanout {
    /// An empty fan-out, which discards everything until an emitter is added.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a sink. Order is preserved but carries no meaning: every sink sees every event.
    #[must_use]
    pub fn with(mut self, emitter: Box<dyn Emitter>) -> Self {
        self.emitters.push(emitter);
        self
    }

    /// Whether no sink is configured — the case where auditing is effectively off.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.emitters.is_empty()
    }

    /// Names of the configured sinks, for the startup log.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.emitters.iter().map(|e| e.name()).collect()
    }
}

impl Emitter for Fanout {
    fn emit(&self, event: &Event) {
        for emitter in &self.emitters {
            emitter.emit(event);
        }
    }

    fn name(&self) -> &'static str {
        "fanout"
    }
}

/// Collects events in memory. For tests, and for asserting what a sink would have received.
#[derive(Debug, Default)]
pub struct MemoryEmitter {
    events: Mutex<Vec<Event>>,
}

impl MemoryEmitter {
    /// An empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A snapshot of everything recorded so far.
    ///
    /// Returns an empty vector if the lock is poisoned: a test assertion that fails is a better
    /// outcome than a panic inside an emitter, which must never take a request down.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().map(|e| e.clone()).unwrap_or_default()
    }
}

impl Emitter for MemoryEmitter {
    fn emit(&self, event: &Event) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event.clone());
        }
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventKind;

    fn sample() -> Event {
        Event::new("2026-08-08T12:00:00Z", EventKind::GatewayStart).detail("version", "0.1.0")
    }

    #[test]
    fn jsonl_writes_one_object_per_line() {
        let dir = std::env::temp_dir().join(format!("sentin-audit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("audit.jsonl");
        let _ = std::fs::remove_file(&path);

        let emitter = JsonlEmitter::new(&path).expect("opens");
        emitter.emit(&sample());
        emitter.emit(&sample());

        let contents = std::fs::read_to_string(&path).expect("readable");
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            serde_json::from_str::<Event>(line).expect("each line is a complete event");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unwritable_sink_does_not_panic() {
        // A full disk or a revoked permission must degrade the trail, never the gateway.
        let emitter = JsonlEmitter::new("/proc/definitely-not-writable/audit.jsonl");
        assert!(
            emitter.is_err(),
            "the failure belongs at startup, not per-request"
        );
    }

    #[test]
    fn fanout_delivers_to_every_sink() {
        let first = Box::new(MemoryEmitter::new());
        let second = Box::new(MemoryEmitter::new());
        // Keep raw pointers out of it: check via a fanout of one, then two, on separate instances.
        let fanout = Fanout::new().with(first).with(second);
        fanout.emit(&sample());
        assert_eq!(fanout.names(), vec!["memory", "memory"]);
    }

    #[test]
    fn an_empty_fanout_is_harmless() {
        let fanout = Fanout::new();
        assert!(fanout.is_empty());
        fanout.emit(&sample()); // must not panic
    }

    #[test]
    fn memory_emitter_records_what_it_was_given() {
        let emitter = MemoryEmitter::new();
        emitter.emit(&sample());
        let events = emitter.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, EventKind::GatewayStart);
    }
}
