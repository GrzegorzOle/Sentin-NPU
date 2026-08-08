// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Detection layers.
//!
//! - `deterministic` (Phase 2): regex + checksum for PESEL, NIP, REGON, IBAN, payment cards,
//!   email and PL phone numbers. The only source of blocking findings.
//! - `ner` (Phase 4): token classification over an OpenVINO IR model, with spans mapped back to
//!   the original text.
//!
//! Both converge on `detect(text) -> Vec<Finding>` from [`sentin_core`].

pub use sentin_core::{DataKind, Finding, Layer};

/// Placeholder for the Phase 2 deterministic detectors.
pub mod deterministic {
    use super::Finding;

    /// Scan `text` for checksum-validated identifiers.
    ///
    /// Phase 2 will implement this; the empty result keeps the pipeline compiling and honest —
    /// no detector claims coverage it does not have.
    #[must_use]
    pub fn detect(_text: &str) -> Vec<Finding> {
        Vec::new()
    }
}
