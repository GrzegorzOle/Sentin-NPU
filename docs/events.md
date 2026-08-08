<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# SIEM event schema

> **Status: implemented.** JSONL, CEF over syslog (UDP) and OTLP/HTTP-JSON emitters all ship, and
> the gateway emits from the real request path. The field set below is binding — any PR that
> changes it updates this file in the same commit (see `CONTRIBUTING.md`).
>
> Verified on a live gateway: one request carrying a PESEL, an email and an IBAN produced three
> `pii_detected` events with three different verdicts plus one `decision_made`, all sharing a
> content hash for correlation, and **no occurrence of any detected value**.

## The rule that governs everything here

**Events carry metadata only. The detected text never appears in an event** — not in any emitter,
not at any log level, not in a debug field. Where the content matters for correlation, a
`content_sha256` of the *whole inspected payload* stands in for it.

This is what makes the gateway safe to point at a SOC: the audit trail cannot itself become the
leak. A field that would let an analyst reconstruct the sensitive value does not belong in the
schema, however convenient.

## Common fields

| Field | Type | Notes |
|---|---|---|
| `ts` | RFC 3339 UTC | event time |
| `event` | enum | see below |
| `detector` | string | e.g. `pesel`, `iban`, `ner_npu` |
| `data_type` | enum | `PESEL`, `NIP`, `REGON`, `IBAN`, `PAYMENT_CARD`, `EMAIL`, `PHONE_PL`, `PERSON`, `ORGANIZATION`, `LOCATION` |
| `target_host` | string | upstream the request was bound for, e.g. `api.anthropic.com` |
| `decision` | enum | `observed`, `advised`, `masked`, `blocked`, `user_override` |
| `content_sha256` | hex | hash of the inspected payload, never the payload |
| `model_id` | string | IR model identifier, empty for layer-1-only events |
| `device` | enum | `NPU`, `GPU`, `CPU` — the device that *actually* executed |

## Event types

| `event` | Emitted when |
|---|---|
| `pii_detected` | any layer produces a finding |
| `decision_made` | the policy engine settles on a verdict |
| `inspection_timeout` | inspection exceeded `inference.timeout_ms`; includes the applied `timeout_policy` |
| `device_fallback` | requested device unavailable or an operator fell back; includes requested vs actual |
| `gateway_start` / `gateway_stop` | lifecycle, with version and resolved device |

## Example

```json
{
  "ts": "2026-08-08T12:00:00Z",
  "event": "pii_detected",
  "detector": "ner_npu",
  "data_type": "PERSON",
  "target_host": "api.anthropic.com",
  "decision": "masked",
  "content_sha256": "sha256:…",
  "model_id": "xlm-roberta-ner-int8-128",
  "device": "NPU"
}
```
