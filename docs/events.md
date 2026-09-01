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
| `client_addr` | string | who sent the request: the caller's IP, taken from the connection and never from a header a caller controls. **No port** - it is ephemeral, so an event carrying it cannot be grouped by caller |
| `upstream_model` | string | the model the caller asked for, e.g. `ovh-llama`, `claude-sonnet-4`, `gemini-2.5-pro` |
| `provider` | string | the adapter that handled it: `anthropic`, `openai`, `google` |
| `source` | enum | `prompt` or `attachment` - where the finding was |
| `attachment_kind` | string | `pdf`, `ooxml`, `text` or `opaque`, from the bytes rather than the declared type |
| `attachment_bytes` | number | decoded size of the attachment |

Every optional field is **omitted** when unset rather than serialised as `null`, so a parser must
treat absence as normal - a layer-1 finding carries no `model_id` and no `device`.

**`model_id` and `upstream_model` are different models and are the pair most likely to be
confused.** `model_id` is the NER model doing the *inspecting*, reported as its IR directory name
(`seq128`); `upstream_model` is the model the data was about to be *sent to*. A dashboard answering
"which model is our data heading towards" reads `upstream_model`; one answering "which detector
version produced this finding" reads `model_id`.

**`client_addr` is personal data in most deployments**, in the same way any proxy log is. It is
recorded because a decision without an owner cannot be acted on - "someone pasted a PESEL" is not
an incident anyone can close. It is an address, not an identity: the gateway performs no user
authentication and does not record credentials, not even a hash of one. A deployment that must not
retain addresses disables the sink rather than filtering the field, because the same value reaches
every emitter.

`upstream_model` is read from the request body's `model` for OpenAI- and Anthropic-shaped calls, and
from the path for Google (`/v1beta/models/<name>:generateContent`). Where neither is present the
field is absent rather than guessed.

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
| `attachment_skipped` | an attachment was present and could not be read: too large, encrypted, or an image. `detail.reason` says which |
| `gateway_start` / `gateway_stop` | lifecycle, with version and resolved device |

## Attachments

Attachments are decoded and read: PDF, `.docx`/`.xlsx`/`.pptx`, OpenDocument, and anything that is
plain text in UTF-8, UTF-16 or a single-byte code page. Findings from inside a document are ordinary
findings and carry no special field - an identifier is an identifier wherever it was written.

Two consequences are worth knowing, because they change what a decision means:

**Every finding says where it was**, in `source`. Two different incidents wear the same clothes
otherwise: somebody typing their own PESEL into a chat is one person's slip, and somebody attaching
a contract that contains one may be sending a file holding many people's data. A rule or a panel
that cannot separate them gives both the same response.

**A finding inside an attachment is never `masked`.** Rewriting bytes inside a PDF or a zip would
corrupt the document, so a detector configured to mask yields `advised` when it fires on an
attachment. `blocked` still works and needs no rewrite, which makes it the honest response to a
checksum-valid identifier in a file about to leave the machine.

**An attachment that could not be read produces `attachment_skipped`, even when the request is
otherwise clean.** An image, an encrypted document or one over `inspect.max_attachment_bytes` is
not a document known to be harmless. Without this event such a request logged `findings=clean` and
emitted nothing at all, which reads as "inspected and fine" and meant "not inspected".

Read `advised` on an attachment accordingly: it means "could not be masked", not "the policy is
lenient". A dashboard counting verdicts without `source` would conclude the second.

**There is no OCR.** A scanned page is an image and its text is not read; it is reported as skipped.
`detail.reason` never contains a filename - a filename carries content, and content does not belong
in an audit trail.

## Example

```json
{
  "ts": "2026-09-01T12:00:00Z",
  "event": "pii_detected",
  "detector": "person",
  "data_type": "PERSON",
  "target_host": "api.anthropic.com",
  "decision": "masked",
  "content_sha256": "sha256:…",
  "model_id": "seq128",
  "device": "NPU",
  "client_addr": "10.1.2.3",
  "upstream_model": "claude-sonnet-4",
  "provider": "anthropic",
  "source": "prompt"
}
```

The same identifier found inside an attached document. Note `advised` rather than `masked`: a PDF
cannot be rewritten, so the verdict a mask-configured detector reaches there is one step lower.

```json
{
  "ts": "2026-09-01T12:00:01Z",
  "event": "pii_detected",
  "detector": "pesel",
  "data_type": "PESEL",
  "target_host": "api.anthropic.com",
  "decision": "advised",
  "content_sha256": "sha256:…",
  "model_id": "seq128",
  "device": "NPU",
  "client_addr": "10.1.2.3",
  "upstream_model": "claude-sonnet-4",
  "provider": "anthropic",
  "source": "attachment",
  "attachment_kind": "pdf",
  "attachment_bytes": 182344
}
```
