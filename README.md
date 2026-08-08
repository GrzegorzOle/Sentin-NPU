# Sentin-NPU

**Local AI privacy gateway on Intel® NPU — advisory DLP for LLM agents with SIEM audit trail (OpenVINO™)**

Sentin-NPU sits between your LLM agents (Claude, Gemini, local models via Ollama)
and their APIs. Before a prompt leaves the device, it detects sensitive data
(PII, corporate identifiers) **locally on the NPU**, advises the user or masks
the data, and emits an audit event to your SIEM. Advisory by design — it blocks
only on unambiguous policy violations.

> ⚠️ **Status: Proof of Concept.** Not production-ready. See [Roadmap](#roadmap).
>
> Working today, end to end: the proxy with adapters for all three provider APIs, both detection
> layers — deterministic checksums *and* NER inference through OpenVINO — request masking and
> blocking, SIEM audit events over CEF/OTLP/JSONL, and release bundles that install on a machine
> with no Rust and no Python.
>
> **The one thing not verified is the thing the project is named after.** Every measurement here
> was taken with `device = CPU`, because the development machine has an AMD NPU that OpenVINO
> cannot address. Intel NPU execution needs a session on Intel hardware — see
> [Help wanted](#help-wanted-npu-reports). Response-side masking is not implemented either;
> responses are inspected and logged, not rewritten.

[![rust](https://github.com/GrzegorzOle/Sentin-NPU/actions/workflows/rust.yml/badge.svg)](https://github.com/GrzegorzOle/Sentin-NPU/actions/workflows/rust.yml)
[![python](https://github.com/GrzegorzOle/Sentin-NPU/actions/workflows/python.yml/badge.svg)](https://github.com/GrzegorzOle/Sentin-NPU/actions/workflows/python.yml)
[![release](https://img.shields.io/github/v/release/GrzegorzOle/Sentin-NPU?include_prereleases&sort=semver)](https://github.com/GrzegorzOle/Sentin-NPU/releases)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

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

![The agent talks to a gateway on localhost; the gateway inspects the request with checksum detectors and NER on the NPU, decides, masks, forwards the masked request to the provider, and sends metadata-only events to the SIEM](docs/architecture.svg)

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

### From a release bundle — no toolchain needed

The bundle carries the gateway, the diagnostics, the OpenVINO runtime and the quantized model.
Nothing else is required: no Rust, no Python, no OpenVINO installation.

```bash
# Linux x64 — pick the newest tag from the releases page
curl -LO https://github.com/GrzegorzOle/Sentin-NPU/releases/latest/download/SHA256SUMS.txt
curl -LO https://github.com/GrzegorzOle/Sentin-NPU/releases/latest/download/sentin-npu-diag-0.0.0.2-linux-x64.tar.gz
sha256sum -c SHA256SUMS.txt --ignore-missing

tar xzf sentin-npu-diag-0.0.0.2-linux-x64.tar.gz
cd sentin-npu-diag-0.0.0.2-linux-x64

./run.sh              # every diagnostic, both shape variants, collected into one archive
./install.sh          # → ~/.local/share/sentin-npu, wrappers in ~/.local/bin

sentin-gateway ~/.local/share/sentin-npu/config.yaml
```

`packaging/systemd/` holds a **user** unit if you want it running as a service. The Windows zip is
built by the same CI job and passes its build, but it has **not been run on Windows yet** — treat
it as untested.

### From source

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

# The OpenVINO runtime is loaded with dlopen at startup, so it has to be on the library
# path. The Python wheel installed above already carries it -- including the NPU plugin.
export OVLIB=$PWD/tools/.venv/lib/python3.11/site-packages/openvino/libs
export LD_LIBRARY_PATH=$OVLIB:$LD_LIBRARY_PATH

./gateway/target/release/sentin-gateway config/default.yaml
```

`dlopen` looks for **unversioned** sonames (`libopenvino_c.so`) and the wheel ships only versioned
ones (`libopenvino_c.so.2630`). If startup fails with *"Unable to find the `openvino_c` library to
load"*, create the symlinks:

```bash
for f in "$OVLIB"/*.so.*; do ln -sf "$f" "${f%%.so.*}.so"; done
```

Without a loadable runtime the gateway still starts — it logs the failure and inspects with
layer 1 only, because a proxy in the path of real work should not refuse to run over a missing
optional component.

The listen address comes from the config file (`listen.host` / `listen.port`, default
`127.0.0.1:4141`). Port 4000 is deliberately avoided: LiteLLM and similar model routers commonly sit there.

Useful extras:

```bash
tools/.venv/bin/python tools/devices.py       # which OpenVINO devices this machine really has
tools/.venv/bin/python tools/validate_model.py --model herbert --seq 128 --dataset wikiann
./gateway/target/release/sentin-gateway --doctor --json my-machine.json  # device report
./gateway/target/release/sentin-bench             # latency (M2a, M2c)
./gateway/target/release/sentin-bench --energy    # energy overhead (M5b)
```

Point your agent at the gateway:

```bash
# Anthropic SDK
export ANTHROPIC_BASE_URL=http://localhost:4141/anthropic

# OpenAI-compatible (Ollama, LM Studio, vLLM)
export OPENAI_BASE_URL=http://localhost:4141/openai
```

For Google GenAI the gateway serves `/google/*`; the SDK has no standard base-URL environment
variable, so pass `http://localhost:4141/google` as the client's endpoint/base-URL option.

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
- Inference runs on its own OS thread, reached by channel, with a configurable timeout. Blocking
  inference on a tokio worker would stall every request that shares it, and a model that fails to
  load leaves layer 1 running rather than taking the gateway down.
- Tested on: AMD Ryzen AI 7 350 / Fedora, OpenVINO 2026.3, **device CPU and GPU only**.
  Intel NPU is **not yet verified** — see below.

Measured with `--doctor` on the dev machine (HerBERT INT8, sequence 128):

| Device | Compile | First inference | Steady |
|---|---|---|---|
| CPU — AMD Ryzen AI 7 350 | 686 ms | 14.3 ms | **11.7 ms** |
| GPU — NVIDIA dGPU via OpenCL | 706 ms | 116.5 ms | 116.0 ms |

The NPU column is missing on purpose. It is the column this project exists to fill.

### Benchmarks

Full detail, methodology and caveats: **[docs/benchmarks.md](docs/benchmarks.md)**.

Model quality (WikiANN, 500 sentences per language, exact span match, PER/ORG/LOC):

| Model | Licence | F1 PL | F1 EN | INT8 size |
|---|---|---|---|---|
| `herbert-base-ner` FP32 | CC-BY-4.0 | **88.06** | 58.97 | — |
| `herbert-base-ner` INT8 | | **87.57** | 59.51 | 123 MB |
| `xlm-roberta-base-ner-hrl` INT8 | AFL-3.0 | 62.62 | 53.36 | 284 MB |

Gateway cost, measured against a local mock upstream so the figures are the gateway's own and not
the network's:

| Metric | Result |
|---|---|
| Deterministic layer throughput | 296 MB/s – 1.22 GB/s |
| Proxy overhead, p95 (no inspection) | **+0.07 ms** |
| Full pipeline overhead, p95 (layer 1 + NER on CPU) | **+10.8 ms** — one inference, and little else |
| Streaming time-to-first-token | +0.1 ms (`passthrough`) · +92 ms (`sliding_window`) · +511 ms (`buffer`) |
| Energy overhead | **+0.57 mJ per request** (≈ 5.7 mW at 10 rps, AMD dev machine) |
| INT8 quality loss | ≤ 0.9 pp F1 |
| Resident memory | 9 MB without the model · 506 MB with it |
| False positives on 1 MB of PII-free prose | 0 |

The Rust engine and the Python reference toolchain are scored against each other on every run of
the test suite; they agree to two decimal places (95.52 PL / 84.44 EN on the committed fixtures).
That is a parity check, not a quality claim — the WikiANN figures above are the honest measure.

Per-device inference latency and power — the NPU-vs-GPU-vs-CPU comparison this project exists to
produce — requires Intel hardware and is not measured yet:

| Device | Latency p50 (ms) | Latency p95 (ms) | Power (W) |
|---|---|---|---|
| NPU | pending | pending | pending |
| GPU (Intel iGPU) | pending | pending | pending |
| CPU | pending | pending | pending |

### Help wanted: NPU reports

The project targets the Intel NPU but is developed on a machine that does not have one, so the
per-device table above stays empty until someone with the hardware runs the check. That is one
command, and it takes a few minutes:

```bash
# from a release bundle — no toolchain, no network, no Python
./run.sh                     # add --power for energy per device

# or from a source build
./gateway/target/release/sentin-gateway --doctor \
    --model models/herbert/int8/seq128/openvino_model.xml --json npu-report.json
```

`run.sh` compiles and executes the real IR at **both** sequence lengths, on every device the
machine exposes, and records the driver, kernel module and device nodes alongside the timings —
an NPU that accepts one shape and refuses the other is exactly the kind of answer this is looking
for. Everything lands in a single archive.

Then open an [npu-report issue](https://github.com/GrzegorzOle/Sentin-NPU/issues/new?template=npu-report.yml)
and paste the result. **A report where the NPU refuses the model is as valuable as one where it
works** — knowing which operators fall back, and why, is the point. The report carries hardware,
driver versions and timings; it never touches the inspection path and contains no processed text.

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

Formats: CEF over syslog (UDP/TCP), OTLP over HTTP (JSON encoding), and JSONL to a file — all
three implemented, configured in `config/default.yaml`, and fanned out to independently. Field
reference: **[docs/events.md](docs/events.md)**, which is authoritative for the schema.

Two properties the implementation guarantees rather than promises: an emitter that fails — full
disk, unreachable collector — is logged and skipped, never propagated into the request; and a
clean request emits **nothing**, because a SIEM full of events about nothing buries the ones that
matter.

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
- **English NER is weak** (~59 F1 on WikiANN). The model is Polish-first, and this is a measured
  property rather than a bug to be filed; layer 1 catches structured identifiers in any language.
- **Polish inflection is not handled specially.** Declined surnames are the known weak spot;
  fine-tuning is a roadmap item.
- **Requests are masked; responses are not.** The response side is inspected and audited, but
  rewriting a stream while it renders is roadmap work, not PoC.
- Checksum detectors accept roughly 3.5 % of *uniform random* 9–19 digit runs, because a mod-11
  check passes about one in eleven by arithmetic. Prose is unaffected — the measured false
  positive count on 1 MB of PII-free text is zero.
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
