// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Detection layers.
//!
//! - [`deterministic`] (layer 1): regex-free single-pass scanning plus checksum validation for
//!   PESEL, NIP, REGON, IBAN, payment cards, email and Polish phone numbers. The only source of
//!   findings that may justify blocking a request — and then only the checksum-backed ones.
//! - `ner` (layer 2, Phase 4): token classification over an OpenVINO IR model, with spans mapped
//!   back to the original text.
//!
//! [`select`] decides which device layer 2 runs on, by timing each one on the real model rather
//! than by walking a fixed preference order.

#![warn(missing_docs)]

pub mod checksums;
pub mod deterministic;
#[cfg(feature = "ner")]
pub mod ner;
pub mod ov;
pub mod select;
pub mod testdata;

pub use deterministic::detect;
pub use sentin_core::{DataKind, Decision, Finding, Layer, Validation};
