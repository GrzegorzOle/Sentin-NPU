# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**This file is the living execution plan.** It absorbed `PLAN.md` (deleted 2026-08-08 — the plan
must never enter git). Phases that finish, research questions that get answered, and measurements
that get taken are recorded *here*. Keep it current when you finish work; a stale plan is worse
than none.

---

## 0. Hardware context — READ BEFORE STARTING

**Dev machine (primary):** Fedora Linux · AMD Ryzen AI 7 350 (NPU = AMD XDNA) · NVIDIA RTX 5070.
**Test machines (secondary):** Intel Core Ultra (NPU) on Windows 11, and on Linux.

### The critical constraint

**The OpenVINO NPU plugin supports Intel NPUs only.** The dev machine's AMD XDNA NPU is not
visible to OpenVINO as an NPU device, and the NVIDIA GPU is not an OpenVINO target either.

Consequences, all of which bind from the first commit:

1. All development on this machine runs with **device = CPU** (the OpenVINO CPU plugin works fine
   on AMD).
2. **Device selection must be configurable from day one**: `--device NPU|GPU|CPU|AUTO`, with
   automatic fallback and a log line stating which device actually executed the inference.
3. The NPU path is verified **only** on the Intel test machines. Any phase whose exit criterion
   says "works on NPU" needs a session on Intel hardware — do not claim it from a CPU run.
4. The NVIDIA 5070 may serve as a comparative reference (ONNX Runtime + CUDA) in benchmarks —
   optional, low priority.
5. AMD XDNA via ONNX Runtime + Vitis AI EP is out of PoC scope — roadmap only.

**The user already runs a native NPU inference engine** (the `NPU-*` models behind the LiteLLM
router, on `gofedora:8080/v1`). It is *not* a substitute and must not be treated as one: the
project's subject is OpenVINO on Intel NPU, and a different runtime on different silicon cannot
support a claim about either. Its legitimate use is as a comparison baseline — "is NPU inference
worth it at all" — never as evidence for an OpenVINO result.

**This is why the project is built the way it is.** Everything is developed device-agnostically on
hardware that cannot run the target, so that the eventual Intel session is a short scripted
checklist rather than a debugging expedition on borrowed hardware. Anything that can be settled
without an Intel NPU is settled first, and the Intel-only list is kept deliberately short (see the
end of §2).

### Measured device inventory on the dev machine (2026-08-08, OpenVINO 2026.3.0)

```
available_devices → ['CPU', 'GPU']
CPU  = AMD Ryzen AI 7 350 w/ Radeon 860M            — compiles + executes
GPU  = NVIDIA GeForce RTX 5070 Laptop GPU (dGPU)    — compiles + executes
```

Reproduce with `tools/.venv/bin/python tools/devices.py`. Full detail in `docs/npu-compat.md`.

**Correction to the original plan (B0, closed):** the plan asserted the NVIDIA GPU is not an
OpenVINO target. It is, on this machine. OpenVINO's GPU plugin enumerates through the OpenCL ICD
loader, the only ICDs installed are `nvidia.icd` and `xilinx.icd`, so it binds NVIDIA's OpenCL
driver (`vendor=0x10de`) — no CUDA involved. A test model compiled and executed correctly.
Caveats that keep this from mattering much: no FP16 in its capability list, only a trivial op pair
proven so far, and Intel does not support the configuration. **Treat dev `GPU` as opportunistic,
never as a stand-in for the Intel iGPU** — M5 comparisons still require one Intel machine. The AMD
Radeon 860M iGPU does not appear at all (no Mesa/rusticl ICD).

---

## 1. Target architecture

Hybrid by design: **Python = model toolchain (offline)**, **Rust = gateway runtime (shipped)**.

```
agent (Claude SDK / Gemini SDK / Ollama, base_url → localhost)
        │ HTTP(S), JSON, SSE
        ▼
┌─────────────────────────────────────────────────────┐
│ sentin-gateway (Rust, tokio + axum/hyper)           │
│  API adapters:                                      │
│    /anthropic/*  → api.anthropic.com                │
│    /openai/*     → localhost:11434 (Ollama) / other │
│    /google/*     → generativelanguage.googleapis.com│
│  inspection pipeline (request + response):          │
│    L1 deterministic  (Rust, regex + checksum)       │
│    L2 NER            (openvino crate → IR model)    │
│    L3 policy engine  (advise / mask / block)        │
│  audit: CEF/syslog + OTLP emitters                  │
│  (metadata + hash — never content)                  │
└─────────────────────────────────────────────────────┘

toolchain (Python, offline, tools/):
  HF model → optimum-intel → OpenVINO IR → INT8 quantization → validation
```

### Repo layout

```
gateway/            # Rust workspace (runtime)
  crates/
    sentin-core/    # pipeline, event types, policies
    sentin-detect/  # L1 deterministic + L2 bridge to OpenVINO
    sentin-proxy/   # API adapters, SSE streaming
    sentin-audit/   # CEF/OTLP emitters
tools/              # Python (model toolchain, benchmarks)
  prepare_model.py · quantize.py · validate_model.py · bench/
models/             # gitignored — IR ships via GitHub Releases, not the repo
config/default.yaml
docs/               # events.md · benchmarks.md · npu-compat.md
tests/fixtures/     # synthetic data ONLY, valid checksums
```

### Invariants — code violating these is wrong even if tests pass

- **On-device only.** No content leaves the machine for inspection, ever.
- **Advisory first.** Only L1, on a checksum-valid match, may `block`. L2/L3 advise or mask; the
  user decides.
- **Audit without content.** Events carry `ts, event, detector, data_type, target_host, decision,
  content_sha256, model_id, device` — never the detected text. Schema changes update
  `docs/events.md` in the same PR.
- **NPU-first, CPU-fallback**, transparent; an unsupported op must not fail the request.
- Rust: edition 2021+, `clippy -D warnings`, `rustfmt`.
- Python: 3.11+, `ruff format` + `ruff`, type hints on public functions.
- Apache 2.0 header in new source files.
- **DCO sign-off on every commit** (`git commit -s`). Unsigned commits cannot be merged.
- Test fixtures: synthetic PESEL/NIP/IBAN with *valid* checksums only — never real data.
- **No AI-attribution anywhere in committed material** — no `Co-Authored-By: Claude`, no
  "generated with", no model names in commits, code, or docs. (Naming Claude/Gemini/Ollama as
  *proxied providers* is product functionality and stays.)
- **Do not trust training knowledge for OpenVINO APIs.** The library moves fast; verify every API
  against current documentation. Same for the `openvino` Rust crate.

---

## 2. Phases

Each phase has exit criteria. **Do not advance without meeting them.** Phases marked 🔬 contain a
research component whose result may change the plan — update this file when it does.

### Phase 0 — Repo and environment foundation — COMPLETE (2026-08-08)

Delivered in commit "Add Phase 0 foundation":
- Cargo workspace `gateway/` with `sentin-core` (shared types + the advisory-first invariant
  enforced by `Layer::max_decision` / `Finding::clamp_decision`, 3 unit tests) and stub
  `sentin-detect` / `sentin-proxy` / `sentin-audit`.
- `tools/.venv` on Python 3.11.15; `requirements.txt` (floors) + `requirements.lock.txt`
  (resolved pins). torch from the CPU index. **`transformers` is deliberately unconstrained** —
  optimum-intel pins `<5.6` and an independent floor makes resolution unsolvable.
- `tools/devices.py` — device inventory that *probes* each device by compiling and executing,
  not just enumerating. Output feeds `docs/npu-compat.md` and community npu-report issues.
- `config/default.yaml`, `docs/{events,benchmarks,npu-compat}.md`, `.gitignore`.
- CI workflows (rust/python/dco) + `.pre-commit-config.yaml`, hooks installed.

**Verified locally:** `cargo build`/`fmt`/`clippy -D warnings`/`test` clean on Linux; ruff clean;
all pre-commit hooks pass; OpenVINO sees and executes on CPU.

**Verified in CI** (pushed as `3385392`): the `rust` workflow passed on **both** ubuntu-latest and
windows-latest — fmt, clippy, build and test, with the 3 `sentin-core` tests passing on Windows.
The `python` workflow passed. All the action versions guessed without network access
(`actions/checkout@v5`, `actions/setup-python@v6`, `astral-sh/ruff-action@v3`,
`dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`) turned out to resolve.

**Still unproven:** the `dco` workflow triggers only on `pull_request`, so it has never executed.
It will first run — and may first fail — on the project's first PR.

### Phase 1 — Model toolchain (Python) — COMPLETE (2026-08-08)

Delivered: `tools/{model_registry,prepare_model,quantize,validate_model,fixtures,wikiann}.py`,
50 synthetic PL/EN fixtures, B1 closed, `NOTICE.md` updated with the CC-BY-4.0 model licence.

Exit criteria met: `prepare_model.py --model herbert` produces both IR variants from one command;
M3 passes on every variant (worst ΔF1 −0.90 pp); the decision is documented.

Things worth not rediscovering:
- **`protobuf` and `tiktoken` are both required** to convert sentencepiece-only tokenizers. Without
  protobuf, transformers falls back to a TikToken parser and dies with a *misleading* "tiktoken is
  required" error on a perfectly valid sentencepiece file.
- **HerBERT's IR declares `token_type_ids`, but its tokenizer does not emit them.** Missing inputs
  must be supplied explicitly as zeros. The FP32 graph silently tolerated the omission; the INT8
  graph failed with an opaque eltwise shape error. Phase 4's Rust bridge must do the same.
- **Span reconstruction:** the entity *label* comes from a word's first subword, but its *character
  extent* must cover every subword. Getting this wrong truncates entities mid-word
  ("Marka Wiśniowieckiego" → "Marka Wiśni") and cost 63 F1 points before it was found.
  `validate_model.py` is the reference implementation Phase 4 must reproduce.
- The committed fixtures are too small for threshold decisions (~21 EN entities → ~5 pp per miss).
  Use `--dataset wikiann` for anything quantitative.
- **Quantization destroys static shapes, and that would have looked like an NPU defect.**
  `OVQuantizer.quantize()` returns `[?,?]` inputs even when the source IR was reshaped to
  `[1,seq]`. Static shapes are an NPU requirement, so the model would have been rejected on Intel
  hardware and the symptom would have read as "the NPU cannot run our model". `quantize.py` now
  calls `restore_static_shape()` after quantizing and **fails loudly** if the model is still
  dynamic. Found by `--doctor` on a machine with no NPU at all — see `docs/npu-compat.md`.
- **Never re-save an IR into the directory it was read from.** OpenVINO keeps the `.bin` mapped;
  saving in place truncates it and leaves an IR that no longer parses (`Unable to read the
  model … Available frontends: ir jax onnx …`), which looks like a corrupt download. Write to a
  sibling directory and swap. `save_pretrained` also omits the tokenizer and `config.json`.
- Current INT8 numbers after the reshape fix: F1 87.57 PL / 59.51 EN, ΔF1 −0.49 / +0.54 pp.

Toolchain commands:

```bash
tools/.venv/bin/python tools/prepare_model.py --model herbert          # HF → IR, seq 128 + 512
tools/.venv/bin/python tools/quantize.py      --model herbert          # → INT8
tools/.venv/bin/python tools/validate_model.py --model herbert --seq 128 --dataset wikiann
```

### Phase 2 — Deterministic layer L1 (Rust) — COMPLETE (2026-08-08)

Delivered: `sentin-detect` with `checksums` (PESEL incl. date, NIP, REGON 9/14, IBAN mod-97,
Luhn), `deterministic` (single-pass scanner), and `testdata` (synthetic generators). 33 tests
including proptest; criterion bench; M1 and M7 measured.

Exit criteria met: every valid synthetic identifier is detected, nothing with a broken checksum
is; M1 = 296-1252 MB/s against a 100 MB/s threshold; M7 = 0 findings in 1 MB of PII-free prose.

Design decisions that are load-bearing:
- **`Validation::{Checksum, Pattern}` was added to `sentin-core`.** `Layer` alone could not express
  the advisory-first invariant: email and phone are found by L1 but have no checksum, so
  `Finding::max_decision` takes the stricter of layer and evidence. Pattern-only findings can mask,
  never block — enforced in types, not config.
- **Spans are byte offsets** (ASCII patterns, so always on char boundaries). Phase 4 must convert
  the tokenizer's *character* offsets before building findings.
- **Token boundaries are mandatory.** Without them an 11-digit window of a 16-digit card passes the
  PESEL checksum about one time in ten.
- **Cards need a known issuer prefix on top of Luhn.** Luhn alone accepts ~1 random string in 10.
- **IBAN requires the canonical `XX00` opening and 4-character grouping.** The permissive first
  version swallowed whole sentences — "of 5887 units left the depot at 14" is 34 characters that
  pass mod-97. M7 caught it; the fix also took clean-prose throughput from 335 MiB/s to 1.22 GiB/s,
  because ordinary words are now rejected on the first byte.

**Known limit, documented not hidden:** checksum detectors have a ~3.55 % false-positive rate on
uniform random 9-19 digit runs, because mod-11 checks accept roughly 1 in 11 by arithmetic. Prose
is unaffected. This is the ceiling of what L1 can promise.

### Phase 3 — Gateway proxy (Rust) — COMPLETE (2026-08-08)

Delivered: `sentin-proxy` with `config` (YAML), `adapters` (three provider schemas),
`inspect` (detect → decide → mask), `stream` (three SSE strategies), `mock` (test/bench upstream),
the `sentin-gateway` binary and the `sentin-bench` harness. 33 tests; M2a and M2c measured; B2 closed.

Exit criteria met: three adapters work e2e against the mock, streaming works under all strategies,
M2a = +0.07 ms p95 (threshold 5 ms), B2 documented.

Decisions worth keeping:
- **Adapters return JSON pointers, not parsed structs.** Deserialising into a common shape and
  re-serialising would silently drop provider fields the gateway does not know about. Pointers let
  the body be forwarded exactly as the caller wrote it, with only the text locations rewritten.
  A test asserts `temperature` and unknown vendor extensions survive masking.
- **Two independent ceilings on every decision:** what the operator configured, and what the
  evidence supports. `mode: block` on `email` yields masking, not blocking — tested.
- **Unparseable or non-JSON bodies are forwarded, not rejected.** The gateway sits in the path of
  real work; failing closed on a parse error is worse than not inspecting.
- **Unknown detectors default to `Observed`.** A detector added in code but missing from config
  must not silently start blocking traffic.
- The **mock upstream records what it received**, so e2e tests assert on *what crossed the
  boundary* rather than on the gateway's own claims. That is the test that would catch a gateway
  masking the copy it shows the user while forwarding the original.

**Exit criterion also met against real infrastructure, not only the mock (2026-08-08):** client →
gateway → LiteLLM → OVH AI Endpoints (`Meta-Llama-3.3-70B`). Asked to quote back what stood in
place of the number, the remote model answered *"W miejscu numeru PESEL widzę: [PESEL]."* The
identifier never left the machine. Reproduction and a second model's output in `docs/benchmarks.md`.

**Ports.** The gateway listens on **4141**, not 4000 — LiteLLM (the user's model router) owns 4000
on this machine, and the `openai` provider now points upstream at `http://localhost:4000`. A test
pins the port so config and expectation cannot drift. Router details: see the `litellm-router`
memory note.

Still open for later phases: response-side findings are detected and logged but not masked
(rewriting a stream mid-render is roadmap); `sentin-audit` is not wired in yet (Phase 6).

Commands:

```bash
# The Cargo workspace is in gateway/, so build by manifest path and run from the repo root.
cargo build --release --manifest-path gateway/Cargo.toml
./gateway/target/release/sentin-gateway config/default.yaml   # run the gateway (port 4141)
./gateway/target/release/sentin-bench                         # M2a + M2c
./gateway/target/release/sentin-bench --energy --rps 0        # M5b (saturation)
LD_LIBRARY_PATH=$OVLIB ./gateway/target/release/sentin-gateway --doctor \
    --model models/herbert/int8/seq128/openvino_model.xml --json report.json
```

`$OVLIB` is the OpenVINO `libs` directory with **unversioned symlinks** created (see B3).

### Phase 4 — Rust↔OpenVINO bridge, NER L2 — COMPLETE on CPU (2026-08-08); NPU needs Phase 5

Already built:
- **`sentin-detect::ov`** — device inventory, `AUTO` resolution with NPU>GPU>CPU priority and a
  visible fallback flag, and a compile+execute probe. Unit-tested for the resolution rules.
- **`sentin-gateway --doctor`** (pulled forward from Phase 7, because it is what makes an Intel
  session cheap and what community `npu-report` issues attach). It compiles and runs the real IR
  on every enumerated device and records refusals as results.

First inference numbers on this machine (HerBERT INT8 seq128, via `--doctor`):

| Device | Compile | First infer | Steady |
|---|---|---|---|
| CPU (Ryzen AI 7 350) | 686 ms | 14.3 ms | **11.7 ms** |
| GPU (NVIDIA via OpenCL) | 706 ms | 116.5 ms | 116.0 ms |

That 11.7 ms is the input to M2b: L2 on CPU has roughly 12 ms of headroom against the 150 ms
budget, so the pipeline should pass comfortably once the NER path is wired.

**`sentin-detect::ner` is built and runs against the real IR** (5 integration tests, which *skip
with a printed reason* when the model or OpenVINO libraries are absent — that is the normal state
in CI).

The trap it hit, worth remembering because it is counter-intuitive: **the `tokenizers` crate
reports byte offsets in Rust, but character offsets through its Python bindings** — each language
indexing strings its own way. `validate_model.py` therefore converts and `ner.rs` must not.
"Zażółć Anna" puts `Anna` at chars 7..11 and bytes 11..15; applying the Python-side reasoning here
produced empty spans, caught by an integration test on Polish text.

Note for measurement on this machine: `AUTO` resolves to **GPU**, which here is the NVIDIA card at
~116 ms versus ~12 ms on CPU. The priority order (NPU>GPU>CPU) is right for Intel, where GPU means
the iGPU; measure M2b with an explicit `--device CPU` here or the number describes the wrong device.

**Layer 2 is wired into the proxy and M2b passes.** `ner_service` owns the engine on a dedicated
OS thread (inference is blocking, tens of ms — running it on a tokio worker would stall every
request sharing that thread), reached by channel with a timeout whose policy is configurable;
default fail-open, because the gateway sits in front of real work. A model that will not load logs
a warning and leaves layer 1 running rather than stopping the gateway.

Verified live: `findings=pesel:Masked,person:Advised,location:Observed` on one request — L1 and L2
findings merge, each clamped by its own configured mode.

M2b on CPU: **+10.8 ms p95** against a 150 ms budget. Matches the 11.7 ms steady inference from
`--doctor`, i.e. the pipeline costs one inference and little else.

Still to do in this phase: M4 scored from the Rust path (the F1 numbers in `docs/benchmarks.md`
were measured by `validate_model.py`; the Rust engine is only known to find the right entities on
the integration fixtures, not scored). NPU numbers need Phase 5.

**Crate limitation to plan around:** `openvino` 0.11 exposes neither `query_model` nor properties
on a compiled model, so **operator-level fallback lists cannot be produced from Rust**. Phase 5's
"which operators fell back" must come from the Python toolchain.

Facts to carry into Phases 5 and 7:
- The **Python wheel already ships `libopenvino_intel_npu_plugin.so`** — the NPU plugin travels
  with the runtime, so an Intel machine needs the kernel driver but not a separate OpenVINO install.
- `runtime-linking` uses dlopen and looks for **unversioned** sonames. The wheel provides only
  `libopenvino_c.so.2630`, so packaging must create `libopenvino_c.so` symlinks or the binary will
  fail at startup with "Unable to find the `openvino_c` library to load".
- Tokenization in Rust (HF `tokenizers` crate) matching the Phase 1 model.
- `sentin-detect::ner`: load IR → infer → BIO decoding → `Vec<Finding>` with spans mapped back to
  the original text (watch post-tokenization offsets).
- Device selection: `AUTO` with NPU>GPU>CPU priority, `executing on: X` log, and a metric for which
  layers fell back.
- Batching/queueing: inference must not block the proxy — separate task, mpsc channel, timeout with
  configurable fail-open/fail-closed (PoC default: fail-open + `inspection_timeout` audit event).
- Measure M2 (e2e latency with L2 on CPU) and M4 (NER quality on fixtures).

**Exit:** L1+L2 pipeline works on dev (CPU) under load; p95 under M2b; device architecture
NPU-ready with config changes only, no code changes.

### Phase 5 — NPU validation on Intel hardware 🔬 (2d + uncertainty; needs test-machine sessions)
**This is the core of the article material.**
- Test machine setup: NPU driver (Win: Intel package; Linux: intel-npu-driver + Level Zero); verify
  `available_devices` contains NPU. Document **driver versions** in `docs/npu-compat.md`.
- Run both IR variants (128/512) on NPU. Per variant record: does it compile for NPU, which
  operators fall back to CPU (OV can be queried), model compile time, first-inference time.
- 🔬 **B4 — NPU characterization.** Full M2/M5 run for device ∈ {NPU, GPU(iGPU), CPU} on the *same*
  Intel machine: p50/p95 latency, throughput, **package power** (Linux: turbostat/RAPL; Windows:
  HWiNFO log + Intel PCM). Scenarios: idle, single requests, 10 rps stream.
  **Deliverable: table + power chart — the article's central result.**
- If the NPU rejects the model: document why (**that is also a result**), try workarounds (other
  shapes, newer OV, backup model from B1), and file feedback with the OpenVINO community.
- Windows: functional test of the gateway build.

**Exit:** NER runs on NPU **or** a documented analysis of why not, plus a working GPU/CPU variant on
the Intel machine; complete M2/M5 data for three devices in `docs/benchmarks.md`.

### Phase 6 — SIEM audit — COMPLETE (2026-08-08)

`sentin-audit` carries the event types, a JSONL sink and CEF-over-syslog; `sentin-proxy::otlp`
adds OTLP over HTTP with **JSON** encoding, which the spec allows and which keeps protobuf codegen
and a gRPC stack out of the gateway. `audit_sink` builds the fan-out from config and emits from
the real request path.

Design points that matter:
- **No event can carry request text**, because `Event` has no field that would hold it. Asserted
  for JSON, for CEF and end-to-end through the gateway.
- `content_sha256` covers the **whole payload**, not the finding: hashing an eleven-digit
  identifier would be reversible by enumeration.
- **Emitters never fail a request.** A full disk or unreachable collector is logged and skipped.
- A clean request emits **nothing** — a SIEM full of events about nothing buries the real ones.
- SHA-256 is vendored, not a dependency.
- Audit records the upstream **host**, never the full URL: a query string can carry content.

Verified live: PESEL/EMAIL/IBAN in one request produced three `pii_detected` with three different
verdicts plus one `decision_made`, sharing a hash, with zero occurrences of any detected value.

### Original Phase 6 plan
- `sentin-audit`: event schema in `docs/events.md` — `pii_detected`, `decision_made
  {masked|allowed|blocked|user_override}`, `inspection_timeout`, `device_fallback`,
  `gateway_start/stop`.
- Emitters: CEF over syslog (UDP/TCP), OTLP/gRPC, JSONL file.
- Optional demo: docker-compose with Wazuh or Elastic; one console screenshot for the article.

**Exit:** e2e-test events visible in JSONL and received by a syslog listener; `docs/events.md` complete.

### Phase 7 — Packaging and distribution (2-3d)
- Release build: Rust binary + OpenVINO libraries in one archive (tar.gz Linux, zip Windows);
  `install.sh`/`install.ps1` checking for the NPU driver and reporting available devices.
- IR models as separate GitHub Release assets (not in the repo), with sha256.
- `--doctor`: **already delivered in Phase 4** — devices, driver versions, and a real compile+run
  probe per device, with `--json` for community npu-compat reports.
- **Blocker for a portable binary:** a static `x86_64-unknown-linux-musl` build fails on
  `aws-lc-sys` needing a C toolchain for musl. Either install `musl-gcc` on the build host or
  switch reqwest to `rustls-no-provider` and install the ring provider explicitly. Until then a
  test machine needs a Rust toolchain.
- Systemd unit (Linux) + Windows service instructions — optional in PoC.
- CI: release workflow building artifacts on tag.

**Exit:** a fresh machine (Windows VM + clean Fedora/Ubuntu) runs the gateway from a release
artifact with no Rust/Python installed.

### Phase 8 — Publication material (parallel with 5-7)
- `docs/benchmarks.md` → publication-quality charts (power, latency).
- Recording/screenshots: agent sends PESEL → mask → SIEM event.
- EN article draft (tutorial + benchmark + honest limitations).
- "npu-compat report" issue template — community call.
- README: fill in the TODOs (commands, model, benchmarks).

---

## 3. Metrics and thresholds

All measurements go to `docs/benchmarks.md` with date, commit, hardware, and versions (OV, driver).
Harness in `tools/bench/`, repeatable with one command.

| ID | Metric | How measured | PoC threshold |
|---|---|---|---|
| M1 | L1 throughput | criterion, MB/s, 1KB/100KB texts | > 100 MB/s |
| M2a | Proxy overhead, no inspection | p50/p95 vs direct API, 1KB payload | p95 < 5 ms |
| M2b | Full pipeline overhead (L1+L2) | p50/p95 added latency | p95 < 150 ms (CPU), < 80 ms (NPU) |
| M2c | Streaming TTFT impact | time to first token vs baseline | decided by B2; always reported |
| M3 | INT8 quality degradation | F1 on validation set, FP32 vs INT8 | ΔF1 < 2 pp |
| M4 | NER quality (PL + EN) | precision/recall/F1, both implementations | **Rust == Python exactly** (95.52 PL / 84.44 EN on fixtures); WikiANN figures in B1 are the ones to quote for quality |
| M5 | Power draw, per inference device | mean package W: idle / 1 rps / 10 rps | no threshold; **NPU vs GPU vs CPU table = the headline result** |
| M5b | **Energy overhead of the gateway itself** | package RAPL, saturation, idle-subtracted; energy **per request**, not watts | **+0.570 mJ/req** on dev (AMD, powersave) ≈ 5.7 mW at 10 rps; **per-machine report, never merged** |
| M6 | Gateway resource use | RSS MB, CPU% idle | **PASS** — 9 MB without model (budget 50); **506 MB with it**, i.e. 4× the 123 MB on disk |
| M7 | L1 false positives | FP on a PII-free corpus (≥1 MB) | 0 for checksum detectors |

Benchmarking rules: ≥5 runs, discard the first (warm-up/model compilation), report median and p95.
Power: subtract idle baseline; note the method per OS. NPU/GPU/CPU comparisons only on the *same*
physical machine.

**Energy measurement (M5, M5b) — read before quoting any wattage.**
- Harness: `cargo run --release -p sentin-proxy --bin sentin-bench -- --energy [--rps N] [--duration S]`.
  Reader lives in `sentin-proxy::energy` (Linux powercap RAPL; works on Intel *and* AMD, the
  `intel_rapl_msr` driver name is historical).
- **RAPL is package-scoped and the NPU has no separate powercap domain.** You cannot read "NPU
  watts". NPU energy is obtained by *differencing* the same workload at `device=NPU` and
  `device=CPU`. Enumerate the domains on each machine and record what was actually present —
  do not assume this generalises across Core Ultra generations.
- `energy_uj` is root-only since the PLATYPUS mitigation. One-off fix:
  `sudo chmod a+r /sys/class/powercap/intel-rapl:*/energy_uj`; persistent fix is a udev rule (the
  harness prints both). It refuses to run rather than reporting zeros.
- Always subtract idle over the *same duration*, and report the workload difference rather than an
  absolute: a laptop package draws several watts doing nothing, which otherwise swamps the signal.
- Windows has no RAPL sysfs. Intel PCM or an HWiNFO CSV log is the fallback; note the method in the
  result, because the two are not interchangeable. **Use the Intel machine's Linux installation for
  energy work**; Windows is for functional verification.
- **M5b results are per-machine and are not merged.** An Intel run reports Intel's overhead, an AMD
  run reports AMD's. Cross-architecture comparison of gateway overhead describes the two CPUs, not
  the gateway. Each run carries a `fingerprint::Machine` (CPU, OS, governor, ACPI profile, AC vs
  battery, backend, domains) so a number is never separated from the conditions that produced it;
  the harness prints comparability warnings *before* measuring rather than after.
- Portable binary is blocked (Phase 7): static musl build fails on `aws-lc-sys` needing a C
  toolchain. Test machines need a Rust toolchain until that is fixed.

---

## 4. Research log

| ID | Question | Phase | Status | Decision |
|---|---|---|---|---|
| B1 | Which NER model: XLM-R vs HerBERT vs spaCy baseline? | 1 | ☑ closed 2026-08-08 | **`pczarnik/herbert-base-ner`** (CC-BY-4.0). Licence screening killed the plan's first choice: wikineural is CC-BY-**NC**, and the WikiANN XLM-R has no licence at all. Of what remained, HerBERT beat XLM-R on Polish by 23.8 pp *and* on English by 5.8 pp, at 2.3× smaller INT8, and ships `tokenizer.json` (saves Phase 4 work). Full table + limitations in `docs/benchmarks.md`. **English quality (~59-64 F1) is weak — a stated PoC limitation, not a bug.** NPU compatibility remains untested; XLM-R is the fallback if HerBERT will not compile for NPU. |
| B2 | SSE streaming inspection strategy | 3 | ☑ closed 2026-08-08 | **`passthrough` default, `sliding_window` opt-in, `buffer` rejected.** All three implemented and measured (M2c): passthrough +0.1 ms TTFT, sliding_window +92 ms, buffer +511 ms. What decided it is scaling, not size — buffer's penalty *is* the generation time (≈24 s for a 2000-token reply), while sliding_window costs one sentence regardless of length. Request-side inspection is the threat model and is free. Numbers in `docs/benchmarks.md`. |
| B3 | `openvino` crate vs custom FFI vs C++ sidecar | 4 | ☑ closed 2026-08-08 | **`openvino` crate 0.11.0 with `runtime-linking`.** Probed empirically: loads the 2026.3 runtime, enumerates CPU/GPU identically to the Python API, reads `DeviceFullName`/`DeviceCapabilities`, and `PropertyKey::Other(..)` accepts arbitrary keys — so NPU-specific properties are reachable in Phase 5 without patching the crate. No custom FFI, no C++ sidecar. `runtime-linking` (dlopen) also suits Phase 7 packaging: no build-time coupling to an OpenVINO install. **Gotcha:** the Python wheel ships only versioned `libopenvino_c.so.2630`; dlopen needs unversioned `libopenvino_c.so` symlinks. |
| B4 | NPU characterization: operators, fallbacks, power, latency | 5 | ☐ open | |
| B5 | (optional) Reference point: ONNX Runtime + CUDA on the 5070 | 5+ | ☐ open | |
| B0 | Why does OpenVINO list a GPU device naming the NVIDIA 5070? | 0 | ☑ closed 2026-08-08 | OpenCL ICD (`nvidia.icd`) → OV GPU plugin binds NVIDIA's OpenCL driver, no CUDA. Compiles and executes. Opportunistic target only; not a substitute for Intel iGPU. Re-verify with the real IR in Phase 4. |

Each closed entry records: context → what was checked → data → decision → impact on the plan.

---

## 5. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| NER model won't compile for NPU | medium | 2 IR variants, backup model from B1, GPU as plan B; a negative result is article material and OV feedback |
| `openvino` crate immature | medium | B3 timeboxed at Phase 4 start; C++ sidecar as escape hatch |
| SSE buffering ruins UX | high | request-only mode in PoC (B2); response-side to roadmap |
| No timely access to Intel machines | ? | everything developed device-agnostically; Phase 5 is deferrable without blocking 6-7 |
| Weak NER quality on Polish inflection | high | report honestly (M4); fine-tuning is roadmap; L1 catches identifiers independently |
| OpenVINO API drift (fast releases) | medium | pin versions; verify against current docs, never from model memory |

---

## 6. Out of PoC scope (roadmap — do not implement without a decision)

Corporate policies (signed dictionaries → semantic embeddings) · response-side inspection and
prompt-injection→exfiltration defense · MCP server mode for Claude Desktop/Code · NER fine-tuning
for Polish inflection · AMD XDNA / ONNX Runtime multi-vendor · TLS agent↔gateway, fleet management,
policy versioning · commercial layer (separate repo, BSL/Elastic license).

---

## 7. Sequence and estimate

```
Phase 0 (0.5d) → 1 (2-3d) → 2 (1-2d) → 3 (3-4d) → 4 (3-5d)
                                            ↓ (when Intel hardware is available)
                                  Phase 5 (2d+risk) → 6 (1-2d) → 7 (2-3d)
                                                      Phase 8 in parallel
```

Total: **15-22 working days** full-time. Phase 5 carries the most uncertainty (hardware + NPU
maturity). Phases 2, 3, 6 are where Claude Code gives the most leverage; Phases 4-5 require
verifying every API against current OpenVINO documentation.

---

## 8. Repo hygiene

- `CLAUDE.md` is a private working note, excluded via `.git/info/exclude` — never commit it.
- `PLAN.md` was folded into this file and deleted; it must not come back into the repo.
- Repo is public: `GrzegorzOle/Sentin-NPU`, Apache 2.0, aimed at the OpenVINO community.
- **`origin` is SSH (`git@github.com:...`), deliberately.** The stored `gh` OAuth token has scopes
  `gist, read:org, repo` — no `workflow` — so an HTTPS push carrying any change under
  `.github/workflows/` is rejected outright. SSH is not subject to OAuth scopes. If the remote is
  ever moved back to HTTPS, `gh auth refresh -h github.com -s workflow` becomes a prerequisite.
- Security issues in the gateway itself (inspection bypasses) → oleksy@cdest.eu, not public issues.
