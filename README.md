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
> **Verified on an Intel NPU as of 2026-08-09** — one machine, a Core Ultra 7 258V, where the
> model runs on the NPU at 78.21 mJ per inference against 724.14 mJ on that machine's CPU, at a
> realistic 10 requests per second. The
> development machine has an AMD NPU that OpenVINO cannot address, so everything else here is
> measured on CPU and every NPU claim rests on that single box — more reports welcome, see
> [Help wanted](#help-wanted-npu-reports). Response-side masking is not implemented; responses are
> inspected and logged, not rewritten.

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

The bundle carries the gateway, the diagnostics, the latency harness, the OpenVINO runtime and the
quantized model. Nothing else is required: no Rust, no Python, no OpenVINO installation. `./run.sh`
collects a device report and pipeline latency for NPU, GPU and CPU into one archive.

The bundle filename carries the release version, so the commands below read the newest tag rather
than hard-coding one — a pasted version number here goes stale the day the next release lands, and
has done.

```bash
# Linux x64
REPO=GrzegorzOle/Sentin-NPU
TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')
BUNDLE="sentin-npu-diag-${TAG#v}-linux-x64.tar.gz"

curl -LO "https://github.com/$REPO/releases/download/$TAG/SHA256SUMS.txt"
curl -LO "https://github.com/$REPO/releases/download/$TAG/$BUNDLE"
sha256sum -c SHA256SUMS.txt --ignore-missing

tar xzf "$BUNDLE"
cd "${BUNDLE%.tar.gz}"

./run.sh              # every diagnostic, both shape variants, collected into one archive
./install.sh          # → ~/.local/share/sentin-npu, wrappers in ~/.local/bin

sentin-gateway ~/.local/share/sentin-npu/config.yaml
```

`packaging/systemd/` holds a **user** unit if you want it running as a service. The Windows zip is
built by the same CI job and now carries the same three binaries as the Linux bundle, so
`run.ps1` collects the device report *and* pipeline latency per device. It has still **never been
executed on Windows** — it compiles and packs, nothing more, so treat it as untested. Energy is
Linux-only regardless: Windows has no RAPL sysfs.

### Just the model

The quantized IR is also published on its own, for loading from your own code or comparing against
your own conversion. Both archives carry the tokenizer, the label map, the attribution required by
the model's CC-BY-4.0 licence, and a README covering the two traps that catch people loading this
IR by hand. **These names have no version in them**, so the URL below keeps working across
releases; `SHA256SUMS.txt` pins the bytes for whichever release you took them from.

```bash
curl -LO https://github.com/GrzegorzOle/Sentin-NPU/releases/latest/download/sentin-npu-model-herbert-int8-seq128.tar.gz
tar xzf sentin-npu-model-herbert-int8-seq128.tar.gz
```

**Quantizing it yourself will not give you these exact weights.** NNCF calibrates over sampled
data, so every run produces a slightly different INT8 model — worth about ±0.2 pp of F1 in
measurements here. The published archive is one such quantization; if you need the numbers in
`docs/benchmarks.md` to line up with what you are running, take the archive rather than rebuilding.

`seq128` is the default and the shape every published latency figure was measured at; a `seq512`
archive is published beside it for longer inputs. Point `inference.model_dir` at the extracted
directory — as an **absolute** path, or it resolves against the working directory and layer 2
quietly stays down. The release bundles keep their own embedded copy, so you do not need this to
run one.

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
# Or skip both: unpack the published IR into models/herbert/int8/seq128 instead (see "Just the
# model" above). Step 1 documents how the model was made; it does not reproduce it byte for byte
# -- INT8 calibration samples data, so your weights will differ slightly from the published ones.

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
- Tested on: AMD Ryzen AI 7 350 / Fedora (CPU + NVIDIA dGPU via OpenCL), and on an **Intel Core
  Ultra 7 258V / Ubuntu 26.04, where the model runs on the NPU** — see below.

Measured with `--doctor` on the dev machine (HerBERT INT8, sequence 128):

| Device | Compile | First inference | Steady |
|---|---|---|---|
| CPU — AMD Ryzen AI 7 350 | 536 ms | 14.1 ms | **11.8 ms** |
| GPU — NVIDIA dGPU via OpenCL | 672 ms | 121.4 ms | 115.8 ms |

![Steady-state inference per device: 11.8 ms on CPU, 115.8 ms on the NVIDIA GPU, and an empty row
for the Intel NPU](docs/charts/device-latency.svg)

The NPU row is empty on purpose. It is the row this project exists to fill.

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
produce. Measured 2026-08-09 on one **Intel Core Ultra 7 258V** (Lunar Lake, Ubuntu 26.04), all
three devices on the same machine, HerBERT INT8 sequence 128:

Energy is the median of five repeats after a discarded warm-up, at the load a gateway in front of
a few agents actually sees — **10 requests per second**, not saturation.

| Device | Steady inference | Added p95 (full pipeline) | Above-idle power @10 rps | Energy per inference @10 rps |
|---|---|---|---|---|
| NPU — Intel AI Boost | 5.9 ms | +3.85 ms | **0.78 W** | **78.21 mJ** |
| GPU — Intel Arc 140V (iGPU) | 2.7 ms | +3.09 ms | 1.53 W | 160.08 mJ |
| CPU — Core Ultra 7 258V | 23.6 ms | +24.97 ms | 6.92 W | 724.14 mJ |

![Energy per inference at 10 rps: 724.14 mJ on CPU, 160.08 mJ on the Intel iGPU and 78.21 mJ on the
NPU](docs/charts/device-energy.svg)

**The NPU is twice as cheap per inference as the iGPU at this load, and nine times cheaper than the
CPU.** Drive all three flat out instead and the two accelerators converge to within 5 % — at
saturation each amortises its fixed cost over as much work as possible, while at a realistic rate
the iGPU still pays to be clocked up and the NPU does not. A background privacy gateway lives at
the realistic rate, which is why that is the row quoted here.

The iGPU is the faster device (2.7 ms against 5.9 ms); the NPU is the cheaper one, and it leaves
the GPU for whatever the user is actually doing. Both IR variants (sequence 128 and 512) compile
and execute on the NPU, with **no operator falling back**, and NER quality on the NPU matches the
CPU to within 0.25 pp F1. Full tables, method and caveats:
[docs/benchmarks.md](docs/benchmarks.md).

### Help wanted: NPU reports

The table above is one machine. The project is developed on hardware that has no Intel NPU at all,
so everything known about NPU behaviour rests on a single Core Ultra 7 258V — a different
generation, driver or shape may well behave differently, and that is exactly what is worth
hearing about. Running the check is one command and takes a few minutes:

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

Established on one Core Ultra 7 258V; a single machine is not a generalisation, which is why the
reports above are still wanted.

- **First compilation for the NPU is slow**: 1.9 s for sequence 128 and **6.1 s for sequence 512**,
  against roughly one second on CPU or iGPU. The Level Zero driver then caches the compiled blob in
  `~/.cache/ze_intel_npu_cache` (329 MB for both variants) and later starts take 38–60 ms. A
  deployment that wipes that cache pays the full compile on every restart.
- **The iGPU is faster than the NPU** for this model — 2.7 ms against 5.9 ms steady. The NPU wins
  on power, not latency. Choose it to stay out of the way, not to go quicker.
- **`/dev/accel/accel0` is `root:render`.** It worked over SSH only because logind grants the seat
  user an ACL; a service account that is not the seat user needs adding to the `render` group.
- A diagnostic that feeds the graph uninitialised tensors makes the NPU hang and report
  `ZE_RESULT_ERROR_DEVICE_LOST`, which is indistinguishable from the device refusing the model.
  That was our own bug and it is fixed — but if you see that error, check your build is current
  before filing it against Intel's driver.
- The dev machine's AMD XDNA NPU is invisible to OpenVINO; all development there runs on CPU.
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
