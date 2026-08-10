<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# SIEM event schema

> **Status: implemented.** JSONL, CEF over syslog (UDP) and OTLP/HTTP-JSON emitters all ship, and
> the gateway emits from the real request path. The field set below is binding - any PR that
> changes it updates this file in the same commit (see `CONTRIBUTING.md`).
>
> Verified on a live gateway: one request carrying a PESEL, an email and an IBAN produced three
> `pii_detected` events with three different verdicts plus one `decision_made`, all sharing a
> content hash for correlation, and **no occurrence of any detected value**.

## The rule that governs everything here

**Events carry metadata only. The detected text never appears in an event** - not in any emitter,
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
| `detector` | string | the **configured detector key**, which is also what the operator writes in `config/default.yaml`: `pesel`, `nip`, `regon`, `iban`, `payment_card`, `email`, `phone_pl`, `person`, `organization`, `location` |
| `data_type` | enum | `PESEL`, `NIP`, `REGON`, `IBAN`, `PAYMENT_CARD`, `EMAIL`, `PHONE_PL`, `PERSON`, `ORGANIZATION`, `LOCATION` |
| `target_host` | string | upstream the request was bound for, e.g. `api.anthropic.com` |
| `decision` | enum | `observed`, `advised`, `masked`, `blocked`, `user_override` |
| `content_sha256` | hex | hash of the inspected payload, never the payload |
| `model_id` | string | **the IR directory name**, e.g. `seq128`; absent for layer-1-only events |
| `device` | enum | `NPU`, `GPU`, `CPU`, `AUTO` - the device that *actually* executed |

Every optional field is **omitted** when unset rather than serialised as `null`, so a parser must
treat absence as normal - a layer-1 finding carries no `model_id` and no `device`.

`detector` and `data_type` are close to redundant today: a layer-2 `PERSON` finding reports
`detector: "person"`. They are kept apart because `detector` names the thing an operator configures
and `data_type` names the class of data, and those stop coinciding as soon as two detectors find
the same class.

**Known limitation of `model_id`.** It is the last path component of `inference.model_dir`, so both
`models/herbert/int8/seq128` and a bundle's `models/seq128` report `seq128`. That identifies the
shape but *not* the model or its precision, which means two models of the same shape are
indistinguishable in the audit trail - the thing this field exists to prevent. Anyone correlating
events across a model change should pin the version some other way until this carries the model
identity.

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
  "detector": "person",
  "data_type": "PERSON",
  "target_host": "api.anthropic.com",
  "decision": "masked",
  "content_sha256": "sha256:…",
  "model_id": "seq128",
  "device": "NPU"
}
```
