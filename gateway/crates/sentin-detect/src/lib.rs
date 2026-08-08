// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Detection layers.
//!
//! - [`deterministic`] (layer 1): regex-free single-pass scanning plus checksum validation for
//!   PESEL, NIP, REGON, IBAN, payment cards, email and Polish phone numbers. The only source of
//!   findings that may justify blocking a request — and then only the checksum-backed ones.
//! - `ner` (layer 2, Phase 4): token classification over an OpenVINO IR model, with spans mapped
//!   back to the original text.

pub mod checksums;
pub mod deterministic;
pub mod testdata;

pub use deterministic::detect;
pub use sentin_core::{DataKind, Decision, Finding, Layer, Validation};
