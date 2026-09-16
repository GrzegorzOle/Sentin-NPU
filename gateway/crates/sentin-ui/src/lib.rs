// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! The Sentin-NPU desktop console.
//!
//! A gateway that inspects traffic is only as good as the policy somebody wrote for it, and the
//! policy is a YAML file whose most consequential property is invisible: **a detector the file does
//! not mention falls back to observing**, so it finds the identifier, records it, and forwards it
//! anyway. That default is correct (code must never start rewriting somebody's traffic on its
//! own), but it means an omission looks exactly like protection from the outside. This console
//! exists so that the person deciding what is protected does not have to learn YAML to find out
//! what is, and does not have to read a 2 000-line audit file to see what happened.
//!
//! Two jobs, and they are the two the gateway itself deliberately does not do:
//!
//! - **[`policy`]** - change what is detected and what happens to it, editing the installed
//!   configuration in place rather than rewriting it, and refusing any setting the pipeline would
//!   silently clamp.
//! - **[`report`]** - turn the JSONL audit trail into one self-contained HTML file. A station with
//!   a SIEM gets this from the SIEM; a station without one currently gets nothing, which is the
//!   gap this closes.
//!
//! [`service`] and [`app`] are the plumbing around them: what is actually running, and the window.

#![warn(missing_docs)]

pub mod app;
pub mod policy;
pub mod report;
pub mod service;
pub mod text;
