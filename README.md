# Sentin-NPU

**Local AI privacy gateway on Intel® NPU — advisory DLP for LLM agents with SIEM audit trail (OpenVINO™)**

Sentin-NPU sits between your LLM agents (Claude, Gemini, local models via Ollama)
and their APIs. Before a prompt leaves the device, it detects sensitive data
(PII, corporate identifiers) **locally on the NPU**, advises the user or masks
the data, and emits an audit event to your SIEM. Advisory by design — it blocks
only on unambiguous policy violations.

> ⚠️ **Status: Proof of Concept.** Not production-ready. See [Roadmap](#roadmap).
>
> Working today: the proxy with adapters for all three provider APIs, the deterministic
> detection layer, request masking and blocking, and the model toolchain that produces the
> OpenVINO IR. **The NER layer is not wired into the gateway yet** — the model is chosen,
> converted and quantized, but layer 2 inference from Rust is in progress, so the running
> gateway currently inspects with layer 1 only. NPU execution is unverified: it needs Intel
> hardware, which is a separate phase.

<!-- TODO: badges: license, CI -->

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

The gateway itself is **Rust** (`gateway/`, a Cargo workspace: `sentin-core`, `sentin-detect`,
`sentin-proxy`, `sentin-audit`) — that is what gets deployed. **Python** (`tools/`) is the offline
model toolchain that converts and quantizes the NER model; it is never in the request path.

**Design principles**

1. **On-device only** — no content ever leaves the machine for inspection.
2. **Advisory first** — the user decides; hard blocks are reserved for
   deterministic, unambiguous violations (e.g. a valid credit card number).
3. **Audit without content** — SIEM events carry metadata (data type, target
   app, decision, hash), never the sensitive text itself.
4. **NPU-first, CPU-fallback** — inference targets the NPU via OpenVINO;
   falls back to CPU/GPU transparently when an operation is unsupported.

## Quick start

The project is hybrid by design: **Python is the offline model toolchain, Rust is the gateway
runtime that ships.** The two steps below reflect that split.

```bash
git clone https://github.com/GrzegorzOle/Sentin-NPU.git
cd Sentin-NPU

# 1. Model toolchain (Python 3.11+, offline) — produces the OpenVINO IR
python3.11 -m venv tools/.venv
tools/.venv/bin/pip install -r tools/requirements.txt
tools/.venv/bin/python tools/prepare_model.py --model herbert   # HF → IR, static shapes 128 and 512
tools/.venv/bin/python tools/quantize.py      --model herbert   # → INT8

# 2. Gateway (Rust, edition 2021, MSRV 1.82).
#    The Cargo workspace lives in gateway/, so build it by manifest path and run the
#    binary from the repo root, where config/ and models/ resolve.
cargo build --release --manifest-path gateway/Cargo.toml
./gateway/target/release/sentin-gateway config/default.yaml
```

The listen address comes from the config file (`listen.host` / `listen.port`, default
`127.0.0.1:4000`); change it there if port 4000 is already taken on your machine.

Useful extras:

```bash
tools/.venv/bin/python tools/devices.py       # which OpenVINO devices this machine really has
tools/.venv/bin/python tools/validate_model.py --model herbert --seq 128 --dataset wikiann
./gateway/target/release/sentin-bench             # latency (M2a, M2c)
./gateway/target/release/sentin-bench --energy    # energy overhead (M5b)
```

Point your agent at the gateway:

```bash
# Anthropic SDK
export ANTHROPIC_BASE_URL=http://localhost:4000/anthropic

# OpenAI-compatible (Ollama, LM Studio, vLLM)
export OPENAI_BASE_URL=http://localhost:4000/openai
```

For Google GenAI the gateway serves `/google/*`; the SDK has no standard base-URL environment
variable, so pass `http://localhost:4000/google` as the client's endpoint/base-URL option.

Your API key is forwarded upstream unchanged and is never written to a log — the gateway is a
proxy, not a credential broker.

## Detection layers

| Layer | What it catches | Method | Verdict allowed |
|---|---|---|---|
| 1. Deterministic — checksum | PESEL (with its embedded date), NIP, REGON, IBAN, payment cards | single-pass scan + checksum | advise / mask / **block** |
| 1. Deterministic — pattern | email, Polish phone numbers | shape only, no checksum | advise / mask — **never block** |
| 2. NER | names, organizations, locations | token classification, OpenVINO IR | advise / mask |
| 3. Corporate policy | company secrets, project codenames | signed policy artifacts (see Roadmap) | — planned |

The split inside layer 1 is enforced in the type system, not by configuration. Blocking a request
needs arithmetic proof, so a detector with no checksum behind it cannot reach that verdict even if
an operator configures `mode: block` for it — the request is masked instead.

## NPU inference

<!-- The core of the project for the OpenVINO community — keep this section honest and detailed -->

- **Model: [`pczarnik/herbert-base-ner`](https://huggingface.co/pczarnik/herbert-base-ner)**
  (CC-BY-4.0, commercial use permitted). Chosen over a multilingual XLM-R candidate on measured
  quality, size and tokenizer availability — see [docs/benchmarks.md](docs/benchmarks.md).
- Conversion: `optimum-intel` → OpenVINO IR, **static shapes** (sequence 128 and 512; static is an
  NPU requirement, not an optimisation), INT8 via NNCF post-training quantization.
- Rust binding: the [`openvino`](https://crates.io/crates/openvino) crate with `runtime-linking`.
- Tested on: AMD Ryzen AI 7 350 / Fedora, OpenVINO 2026.3, **device CPU and GPU only**.
  Intel NPU is **not yet verified** — see below.

### Benchmarks

Full detail, methodology and caveats: **[docs/benchmarks.md](docs/benchmarks.md)**.

Model quality (WikiANN, 500 sentences per language, exact span match, PER/ORG/LOC):

| Model | Licence | F1 PL | F1 EN | INT8 size |
|---|---|---|---|---|
| `herbert-base-ner` FP32 | CC-BY-4.0 | **88.06** | 58.97 | — |
| `herbert-base-ner` INT8 | | **87.92** | 59.77 | 123 MB |
| `xlm-roberta-base-ner-hrl` INT8 | AFL-3.0 | 62.62 | 53.36 | 284 MB |

Gateway cost, measured against a local mock upstream so the figures are the gateway's own and not
the network's:

| Metric | Result |
|---|---|
| Deterministic layer throughput | 296 MB/s – 1.22 GB/s |
| Proxy overhead, p95 | **+0.07 ms** |
| Streaming time-to-first-token | +0.1 ms (`passthrough`) · +92 ms (`sliding_window`) · +511 ms (`buffer`) |
| Energy overhead | **+0.57 mJ per request** (≈ 5.7 mW at 10 rps, AMD dev machine) |
| INT8 quality loss | ≤ 0.9 pp F1 |
| False positives on 1 MB of PII-free prose | 0 |

Per-device inference latency and power — the NPU-vs-GPU-vs-CPU comparison this project exists to
produce — requires Intel hardware and is not measured yet:

| Device | Latency p50 (ms) | Latency p95 (ms) | Power (W) |
|---|---|---|---|
| NPU | pending | pending | pending |
| GPU (Intel iGPU) | pending | pending | pending |
| CPU | pending | pending | pending |

### Known NPU limitations

Not yet established — this needs a session on Intel hardware, and reporting anything here from a
machine without an Intel NPU would be guesswork. What *is* known so far:

- The dev machine's AMD XDNA NPU is invisible to OpenVINO; all development runs on CPU.
- OpenVINO does expose a `GPU` device here, but it is the NVIDIA dGPU reached through the OpenCL
  ICD loader, not an Intel iGPU, and it advertises no FP16. Treated as opportunistic only.
- The OpenVINO Python wheel already ships `libopenvino_intel_npu_plugin.so`, so an Intel machine
  needs the NPU kernel driver but not a separate runtime installation.
- `runtime-linking` loads libraries with `dlopen`, which looks for **unversioned** sonames; the
  wheel provides only versioned ones, so packaging must add the symlinks.

Details and reproduction steps: [docs/npu-compat.md](docs/npu-compat.md).

## SIEM integration

Every detection produces an event — **metadata only, never content**:

```json
{
  "ts": "2026-08-08T12:00:00Z",
  "event": "pii_detected",
  "detector": "ner_npu",
  "data_type": "PERSON",
  "target_host": "api.anthropic.com",
  "decision": "masked",
  "content_sha256": "sha256:…",
  "model_id": "herbert-base-ner-int8-128",
  "device": "NPU"
}
```

`decision` is one of `observed`, `advised`, `masked`, `blocked`, `user_override`. The hash covers
the whole inspected payload and stands in for it — no field may let an analyst reconstruct the
sensitive value, which is what keeps the audit trail from becoming the leak.

Formats: CEF over syslog, OTLP, JSONL. Field reference: **[docs/events.md](docs/events.md)** —
the schema is fixed there and is authoritative; the emitters themselves are still to be built.

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
