// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Provider API adapters and the streaming proxy (Phase 3).
//!
//! Routes `/anthropic/*`, `/openai/*` and `/google/*` to their upstreams, extracting inspectable
//! text from each provider's own message schema. The caller's API key is forwarded verbatim and
//! must never be logged.
