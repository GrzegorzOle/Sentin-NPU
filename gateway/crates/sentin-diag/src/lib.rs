// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Diagnostics and measurement, kept separate from the gateway runtime.
//!
//! This crate exists so the diagnostic tool can be shipped to machines that have no toolchain and
//! no development environment — the situation that actually applies to the Intel test hardware.
//! It therefore avoids the proxy's async stack entirely: `tokio` and `reqwest` are what prevented
//! a static musl build, and a diagnostic that cannot be copied onto the target machine is useless
//! however good its output.

#![warn(missing_docs)]

pub mod doctor;
pub mod energy;
pub mod fingerprint;
