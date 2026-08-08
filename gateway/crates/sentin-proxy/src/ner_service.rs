// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Layer-2 inference, kept off the request path.
//!
//! Inference is blocking, CPU-bound work measured in tens of milliseconds. Running it inline on a
//! tokio worker would stall every other request sharing that thread, so the engine lives on a
//! dedicated OS thread and is reached over a channel.
//!
//! The timeout is the part that matters operationally. This gateway sits in front of real work, so
//! a model that stalls must not become an outage: the default policy is **fail-open** — forward the
//! request uninspected and record that inspection was skipped. Operators who need the opposite
//! guarantee can choose `fail_closed` and accept the availability cost. Silently waiting forever
//! is not among the options.

use std::sync::mpsc;
use std::time::Duration;

use sentin_core::Finding;
use sentin_detect::ner::{NerEngine, NerError};
use tokio::sync::oneshot;

use crate::config::{Inference, TimeoutPolicy};

/// One inference request handed to the worker thread.
struct Job {
    text: String,
    reply: oneshot::Sender<Result<Vec<Finding>, String>>,
}

/// Why layer 2 produced nothing for a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// Inspection did not finish within `timeout_ms`.
    TimedOut,
    /// The worker is gone, or inference itself failed.
    Unavailable(String),
}

/// A running inference worker.
#[derive(Debug)]
pub struct NerService {
    sender: mpsc::Sender<Job>,
    timeout: Duration,
    policy: TimeoutPolicy,
    device: String,
    fell_back: bool,
}

impl NerService {
    /// Load the model and start the worker thread.
    ///
    /// # Errors
    /// Fails if the model cannot be loaded; the caller decides whether that is fatal. It is not
    /// fatal for the gateway — layer 1 still works without layer 2.
    pub fn start(config: &Inference) -> Result<Self, NerError> {
        let engine = NerEngine::load(std::path::Path::new(&config.model_dir), &config.device)?;
        let device = engine.device().to_string();
        let fell_back = engine.fell_back();

        let (sender, receiver) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("sentin-ner".to_string())
            .spawn(move || worker(engine, &receiver))
            .map_err(|err| NerError::OpenVino(format!("cannot start inference thread: {err}")))?;

        Ok(Self {
            sender,
            timeout: Duration::from_millis(config.timeout_ms),
            policy: config.timeout_policy,
            device,
            fell_back,
        })
    }

    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    #[must_use]
    pub fn fell_back(&self) -> bool {
        self.fell_back
    }

    #[must_use]
    pub fn policy(&self) -> TimeoutPolicy {
        self.policy
    }

    /// Run layer 2 over `text`, giving up after the configured timeout.
    ///
    /// # Errors
    /// Returns [`Skipped`] describing why no findings are available. The caller applies the
    /// timeout policy; this function never decides to drop or refuse a request on its own.
    pub async fn inspect(&self, text: &str) -> Result<Vec<Finding>, Skipped> {
        let (reply, answer) = oneshot::channel();
        let job = Job {
            text: text.to_string(),
            reply,
        };
        if self.sender.send(job).is_err() {
            return Err(Skipped::Unavailable("inference thread stopped".to_string()));
        }

        match tokio::time::timeout(self.timeout, answer).await {
            Ok(Ok(Ok(findings))) => Ok(findings),
            Ok(Ok(Err(err))) => Err(Skipped::Unavailable(err)),
            Ok(Err(_)) => Err(Skipped::Unavailable("worker dropped the job".to_string())),
            // The job stays queued and its result is discarded. Cancelling mid-inference is not
            // possible through the OpenVINO C API used here, so the work is paid for either way.
            Err(_) => Err(Skipped::TimedOut),
        }
    }
}

fn worker(mut engine: NerEngine, receiver: &mpsc::Receiver<Job>) {
    while let Ok(job) = receiver.recv() {
        let result = engine.detect(&job.text).map_err(|err| err.to_string());
        // A closed reply channel means the caller timed out and moved on; that is expected under
        // load and must not stop the worker.
        let _ = job.reply.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_model_directory_disables_layer_two_rather_than_failing_startup() {
        let config = Inference {
            model_dir: String::new(),
            ..Inference::default()
        };
        assert!(
            !config.is_enabled(),
            "an unset model_dir must mean 'layer 1 only', not a broken gateway"
        );
    }

    #[test]
    fn starting_with_a_nonexistent_model_reports_an_error() {
        let config = Inference {
            model_dir: "/nonexistent/model/dir".to_string(),
            ..Inference::default()
        };
        assert!(NerService::start(&config).is_err());
    }

    #[test]
    fn the_default_policy_is_fail_open() {
        // The gateway is in the path of real work: a stalled model must not become an outage.
        assert_eq!(Inference::default().timeout_policy, TimeoutPolicy::FailOpen);
    }
}
