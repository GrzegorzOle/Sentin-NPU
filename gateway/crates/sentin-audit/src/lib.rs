// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Audit event emission (Phase 6): CEF over syslog, OTLP/gRPC, and JSONL to file.
//!
//! Events carry metadata only — `ts`, `event`, `detector`, `data_type`, `target_host`,
//! `decision`, `content_sha256`, `model_id`, `device`. The detected text never appears in an
//! event, in any emitter, under any log level.
