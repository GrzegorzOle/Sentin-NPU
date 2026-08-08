# Sentin-NPU

**Local AI privacy gateway on Intel® NPU — advisory DLP for LLM agents with SIEM audit trail (OpenVINO™)**

Sentin-NPU sits between your LLM agents (Claude, Gemini, local models via Ollama)
and their APIs. Before a prompt leaves the device, it detects sensitive data
(PII, corporate identifiers) **locally on the NPU**, advises the user or masks
the data, and emits an audit event to your SIEM. Advisory by design — it blocks
only on unambiguous policy violations.

> ⚠️ **Status: Proof of Concept.** Not production-ready. See [Roadmap](#roadmap).

<!-- TODO: badges: license, CI, Python version -->

---

## Why

- Employees and AI agents leak sensitive data into cloud LLMs — classic DLP
  tools don't see this traffic in a usable way.
- Cloud-based DLP inspection creates the very problem it solves: your data
  leaves the device to be checked.
- Modern AI PCs ship with an NPU that is mostly idle. Low-power, always-on,
  on-device inference is exactly what a privacy gateway needs.
- EU organizations under **NIS2** must demonstrate risk management and incident
  reporting — "prompt leakage" events are currently invisible to most SOCs.

## Architecture

```
┌──────────────┐     ┌──────────────────────────────────────┐     ┌─────────────────┐
│  LLM agent   │     │            Sentin-NPU gateway        │     │   LLM provider  │
│ (Claude SDK, │────▶│                                      │────▶│  api.anthropic  │
│  Gemini SDK, │     │  1. Deterministic layer (regex,      │     │  googleapis     │
│  Ollama, …)  │     │     checksums: PESEL/NIP/IBAN/cards) │     │  localhost:11434│
│ base_url =   │     │  2. NER on Intel NPU (OpenVINO)      │     └─────────────────┘
│  localhost   │     │  3. Policy: advise / mask / block    │
└──────────────┘     │  4. Audit event (no content)         │
                     └──────────────────┬───────────────────┘
                                        │ CEF / OTLP
                                        ▼
                                   ┌─────────┐
                                   │  SIEM   │
                                   └─────────┘
```

<!-- TODO: replace ASCII with a proper diagram (docs/architecture.png) -->

**Design principles**

1. **On-device only** — no content ever leaves the machine for inspection.
2. **Advisory first** — the user decides; hard blocks are reserved for
   deterministic, unambiguous violations (e.g. a valid credit card number).
3. **Audit without content** — SIEM events carry metadata (data type, target
   app, decision, hash), never the sensitive text itself.
4. **NPU-first, CPU-fallback** — inference targets the NPU via OpenVINO;
   falls back to CPU/GPU transparently when an operation is unsupported.

## Quick start

<!-- TODO: verify commands once the code lands -->

```bash
git clone https://github.com/GrzegorzOle/Sentin-NPU.git
cd Sentin-NPU
pip install -r requirements.txt

# Convert / download the NER model to OpenVINO IR
python scripts/prepare_model.py --device NPU

# Run the gateway
python -m sentin.gateway --port 4000 --config config/default.yaml
```

Point your agent at the gateway:

```bash
# Anthropic SDK
export ANTHROPIC_BASE_URL=http://localhost:4000/anthropic

# OpenAI-compatible (Ollama, LM Studio, vLLM)
export OPENAI_BASE_URL=http://localhost:4000/openai

# Google GenAI
# TODO: document Gemini base_url override
```

## Detection layers

| Layer | What it catches | Method | Verdict allowed |
|---|---|---|---|
| 1. Deterministic | PESEL, NIP, IBAN, credit cards | regex + checksum | advise / mask / **block** |
| 2. NER (NPU) | names, organizations, locations | token classification, OpenVINO on NPU | advise / mask |
| 3. Corporate policy | company secrets, project codenames | signed policy artifacts (see Roadmap) | — planned |

## NPU inference

<!-- The core of the project for the OpenVINO community — keep this section honest and detailed -->

- Model: <!-- TODO: e.g. xlm-roberta-based NER, INT8 via optimum-intel -->
- Conversion: `optimum-intel` → OpenVINO IR, static shapes
- Tested on: <!-- TODO: CPU/GPU/NPU generations, driver versions -->

### Benchmarks

<!-- TODO: the money table -->

| Device | Latency p50 (ms) | Latency p95 (ms) | Power (W) |
|---|---|---|---|
| NPU | | | |
| GPU | | | |
| CPU | | | |

### Known NPU limitations

<!-- TODO: which ops fall back to CPU, shape constraints, driver quirks.
     This section is feedback for OpenVINO engineers — be specific. -->

## SIEM integration

Every detection produces an event — **metadata only, never content**:

```json
{
  "ts": "2026-08-08T12:00:00Z",
  "event": "pii_detected",
  "detector": "ner_npu",
  "data_type": "PERSON",
  "target": "api.anthropic.com",
  "decision": "masked_by_user",
  "content_hash": "sha256:…"
}
```

Formats: CEF over syslog, OTLP. Field reference: [docs/events.md](docs/events.md)
<!-- TODO: write docs/events.md -->

## Roadmap

- [ ] Corporate policy artifacts: signed dictionaries → embedding-based
      semantic similarity (locally indexed, never leaves the org)
- [ ] Polish-language NER fine-tuning (inflected surnames)
- [ ] Response-side inspection (prompt-injection → exfiltration path)
- [ ] MCP server mode (advisory tool for Claude Desktop / Claude Code)
- [ ] Policy versioning & expiry
- [ ] TLS between agent and gateway

## Known limitations

- Off-the-shelf NER: detects identities, not "confidentiality" — contextual
  sensitivity is out of scope for the PoC.
- Masking degrades LLM answer quality; that trade-off belongs to the user.
- NPU support depends on hardware generation and driver version.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Contributions require a DCO sign-off
(`git commit -s`).

## License

Apache 2.0 — see [LICENSE](LICENSE) and [NOTICE.md](NOTICE.md).

Sentin-NPU is an independent community project, not affiliated with, endorsed
by, or sponsored by Intel Corporation, Anthropic, or Google. OpenVINO and Intel
are trademarks of Intel Corporation or its subsidiaries.
