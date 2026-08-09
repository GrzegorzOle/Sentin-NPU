<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# Benchmarks

> **Status: complete.** Everything including M5 — the NPU/GPU/CPU power comparison this project
> exists to produce — has numbers. Most metrics were taken on the dev machine (AMD, device CPU);
> the per-device comparisons come from one Intel Core Ultra 7 258V, measured 2026-08-09, and a
> single machine is not a generalisation.

Every result recorded here states: **date, commit, hardware, OpenVINO version, driver version**.
A number without that context is not a result.

The charts are generated from the figures in this document by
`tools/.venv/bin/python tools/bench/plot.py`, which writes `docs/charts/*.svg` and then checks
that each headline number still appears here — so a chart cannot quietly drift away from the
measurement it illustrates. Every chart's values are also printed on its marks and stated in the
table beside it; nothing is readable only as a coloured bar.

## Method

- ≥5 runs; discard the first (warm-up / model compilation); report median and p95.
- Power: subtract the idle baseline from the loaded measurement; note the method per OS
  (Linux: turbostat/RAPL, Windows: HWiNFO log + Intel PCM).
- **NPU/GPU/CPU comparisons only on the same physical machine.** Cross-machine device comparisons
  are not published.
- Harness lives in `tools/bench/`, repeatable with one command.

## Metric targets

| ID | Metric | PoC threshold | Status |
|---|---|---|---|
| M1 | L1 throughput (MB/s) | > 100 MB/s | **PASS** — 296-1252 MB/s, see below |
| M2a | Proxy overhead, no inspection | p95 < 5 ms | **PASS** — +0.07 ms |
| M2b | Full pipeline overhead (L1+L2) | p95 < 150 ms CPU / < 80 ms NPU | **PASS on all three** — +10.8 ms dev CPU; Intel NPU +3.85 ms |
| M2c | Streaming TTFT impact | decided by B2; always reported | **measured** — see B2 |
| M3 | INT8 quality degradation | ΔF1 < 2 pp | **PASS** — see B1 below |
| M4 | NER quality PL+EN | reported, no hard threshold | **measured** — Rust == Python exactly; NPU within 0.25 pp of CPU |
| M5 | Power draw per device | no threshold — **headline result** | **measured**, 5 repeats — at 10 rps: NPU 78.21 mJ, iGPU 160.08, CPU 724.14 per inference |
| M6 | Gateway resource use | RSS < 50 MB without model | **PASS** — 9 MB; 506 MB with the model |
| M7 | L1 false positives | 0 for checksum detectors | **PASS** on prose; see caveat below |

## End-to-end against a real remote model — 2026-08-08

The automated tests prove masking against a mock upstream, where the assertion is on what the mock
recorded. This is the same thing demonstrated the other way round: through a real router, to a
model running on someone else's infrastructure, with the model itself reporting what it received.

Setup: client → Sentin-NPU gateway (`127.0.0.1:4141`, `pesel: mask`) → LiteLLM router
(`127.0.0.1:4000`) → **OVH AI Endpoints**, model `Meta-Llama-3.3-70B-Instruct`.

Prompt sent by the client, containing a synthetic PESEL:

> Zacytuj dosłownie ciąg znaków, który widzisz w tym zdaniu w miejscu numeru PESEL:
> 'Klient o numerze PESEL **44051401359** złożył wniosek.'

Answer from the remote model:

> W miejscu numeru PESEL widzę: **[PESEL]**.

A second model, `ovh-qwen3-32b`, quoted the sentence back in its own reasoning as
`'Klient o numerze PESEL [PESEL] zlozyl wniosek.'`

The gateway logged `findings=pesel:Masked decision=Masked` — the data kind, never the value. The
identifier never left the machine, and a model 70 billion parameters wide on remote infrastructure
can only confirm that it saw the placeholder.

This is the Phase 3 exit criterion satisfied against real infrastructure rather than a mock.

## Gateway proxy (M2a, M2c) and research question B2 — measured 2026-08-08

Conditions: dev machine (AMD Ryzen AI 7 350), Fedora Linux, rustc 1.96.0, release build.
Client, gateway and upstream all on loopback. 500 samples per configuration after 50 discarded
warm-up requests; 25 streams per strategy after one discarded warm-up. Three independent runs of
the whole harness agreed to within ±0.02 ms (M2a) and ±0.6 ms (M2c).

Reproduce with:

```bash
cargo run --release -p sentin-proxy --bin sentin-bench
```

**Why a mock upstream rather than a real provider.** M2a is trying to resolve an overhead of
tens of microseconds. A round trip to `api.anthropic.com` varies by tens of *milliseconds* — three
orders of magnitude more — so a measurement against the real API would report network weather, not
gateway cost. The mock removes that variable, needs no API key, and makes the result reproducible
by anyone who clones the repo.

### M2a — request-path overhead (threshold p95 < 5 ms)

~1 KB payload, non-streaming, no identifiers in the text.

| Configuration | p50 (ms) | p95 (ms) |
|---|---|---|
| Direct to upstream (baseline) | 0.038 | 0.053 |
| Via gateway, inspection off | 0.075 | 0.121 |
| Via gateway, L1 inspection on | 0.075 | 0.110 |

**Added p95: +0.07 ms — PASS, with roughly 70× headroom against the 5 ms threshold.**

The L1-on and L1-off columns are indistinguishable, and that is expected rather than suspicious:
scanning 1 KB at the measured layer-1 throughput takes about a microsecond, against ~70 µs of
proxy overhead. The difference between those two rows is run-to-run noise, not a speed-up from
enabling inspection.

### M2b — full pipeline with layer 2 (threshold p95 < 150 ms on CPU)

Device **CPU explicitly**, not `AUTO`: on this machine `AUTO` resolves to the NVIDIA dGPU reached
through the OpenCL ICD, which runs the same model in ~116 ms against ~12 ms on CPU. The priority
order (NPU > GPU > CPU) is right for Intel, where `GPU` means the iGPU, but here it would have
measured the wrong device.

HerBERT INT8, sequence 128, payload containing a person, a location and an organisation.

| Configuration | p50 (ms) | p95 (ms) |
|---|---|---|
| Via gateway, layer 1 only | 0.063 | 0.127 |
| Via gateway, layer 1 + layer 2 | 9.286 | 10.872 |

**Added p95 versus direct: +10.8 ms — PASS, roughly 14× headroom against the 150 ms budget.**

![Added p95 latency: +0.07 ms for the proxy alone and +10.8 ms with both detection layers, against
a 150 ms budget](charts/latency-budget.svg)

The figure tracks the steady-state inference time `--doctor` reports for the same model (11.8 ms),
which is the reassuring outcome: the pipeline costs one inference and almost nothing else. Layer 2
runs on a dedicated thread reached over a channel, so it does not block the tokio workers, and a
timeout is resolved by operator policy — fail-open by default, because a stalled model must not
become an outage.

**Reproducing this on another device.** The harness takes the device and the model directory, so
the same metric can be taken for NPU, GPU and CPU on one machine:

```bash
sentin-bench --device NPU --model-dir /path/to/models/seq128 --m2b-only
```

`--m2b-only` drops the streaming section, which does not depend on the inference device. A release
bundle carries `sentin-bench` and `./run.sh` already runs all three devices, writing one
`results/bench-m2b-<device>.json` each.

**Read `device_used`, not `device_requested`.** Asking for a device you do not have does not fail —
it falls back, and on this machine `--device NPU` returns the dGPU's ~119 ms looking every bit like
an NPU measurement. The harness prints both, warns when they differ, and scores the result against
the budget for the device that actually ran (80 ms for NPU, 150 ms otherwise).

#### M2b on all three devices — Intel Core Ultra 7 258V, 2026-08-09

The same metric taken on one machine that has an NPU, an Intel iGPU and a CPU, so the three rows
are comparable. HerBERT INT8 sequence 128; `device_used` equals `device_requested` in every row.

| Device | L1 only p95 | L1 + L2 p50 | L1 + L2 p95 | Added p95 | Budget | Verdict |
|---|---|---|---|---|---|---|
| NPU — Intel AI Boost | 0.158 ms | 3.643 ms | 3.913 ms | **+3.85 ms** | 80 ms | PASS |
| GPU — Arc 140V iGPU | 0.142 ms | 2.750 ms | 3.139 ms | **+3.09 ms** | 150 ms | PASS |
| CPU — Core Ultra 7 258V | 0.108 ms | 24.208 ms | 25.030 ms | **+24.97 ms** | 150 ms | PASS |

The seq512 variant on the NPU adds **+12.75 ms p95**, also a pass. Every device clears its budget
with room to spare, so latency is not what distinguishes them — energy is, and that comparison is
under [M5](#m5-results--per-inference-device-b4--measured-2026-08-09).

Note that the dev machine's CPU figure (+10.8 ms) is *better* than this machine's (+24.97 ms): the
Ryzen AI 7 350 has more cores than the 8 of a Core Ultra 7 258V. Comparing CPU rows across the two
machines describes the two CPUs, not the gateway.

### M2c — streaming, and the decision for B2

The mock emits 40 SSE events 12 ms apart (~0.5 s of generation), ending a sentence every eighth
event — a short answer from a fast local model.

| Strategy | TTFT p50 (ms) | TTFT p95 (ms) | Total p50 (ms) | TTFT vs baseline |
|---|---|---|---|---|
| Direct to upstream (baseline) | 0.3 | 0.4 | 511.0 | — |
| Gateway, `passthrough` | 0.4 | 0.6 | 510.8 | **+0.1 ms** |
| Gateway, `sliding_window` | 92.5 | 93.2 | 511.3 | **+92 ms** |
| Gateway, `buffer` | 511.0 | 512.9 | 511.0 | **+511 ms** |

![Time to first token by strategy: 0.4 ms for passthrough, 92.5 ms for sliding_window, 511 ms for
buffer, against a 0.3 ms baseline](charts/streaming-ttft.svg)

Total time is unchanged in every case — no strategy makes generation slower. What changes is
*when the user sees the first character*, and that is the entire user experience of a streaming
answer.

**The decisive point is not the size of each penalty but how it scales.**

- `buffer` costs the **whole generation time**. Here that is 511 ms because the mock answer is
  short. For a 2 000-token reply at the same rate it would be roughly 24 seconds of blank screen.
  The penalty is unbounded in response length, which makes it unusable for interactive work
  regardless of how safe it is.
- `sliding_window` costs **one sentence**, whatever the answer's total length: 8 events × 12 ms ≈
  96 ms predicted, 92 ms measured. Bounded, and roughly constant.
- `passthrough` costs nothing measurable, and inspects the request only.

**B2 decision: `passthrough` is the PoC default; `sliding_window` is the supported opt-in;
`buffer` is implemented for this comparison but recommended against.** Request-side inspection is
what the threat model is actually about — data leaving the device — and it is free. Response-side
inspection buys defence against a model echoing sensitive data back, which matters mainly for the
prompt-injection→exfiltration path that is explicitly roadmap, and `sliding_window` prices that at
about one sentence of latency for operators who want it.

All three strategies are covered by a test asserting they deliver **byte-identical** responses;
they may differ in *when* bytes arrive, never in what arrives.

Caveat: response-side findings are currently detected and logged, not masked. Rewriting a stream
the client is already rendering is out of PoC scope. The latency above is therefore the cost of
*detection*, which is the part that governs the design choice; masking would add rewriting on top.

### Per-device inference on the dev machine — measured 2026-08-08

From `sentin-gateway --doctor`, which compiles and executes the real IR on every enumerated device
rather than reporting what the device claims to support. Raw report: `docs/doctor-dev-amd.json`.
HerBERT INT8, sequence 128, OpenVINO 2026.3.0.

| Device | Full name | Compile | First inference | Steady |
|---|---|---|---|---|
| CPU | AMD Ryzen AI 7 350 | 536 ms | 14.1 ms | **11.8 ms** |
| GPU | NVIDIA RTX 5070 Laptop (dGPU, via OpenCL) | 672 ms | 121.4 ms | **115.8 ms** |

Re-measured 2026-08-09 with the corrected probe (see the false-negative note above). **The steady
figures reproduced to the decimal** — 11.8 ms and ~116 ms — which is the useful confirmation that
the uninitialised inputs cost nothing in compute time and that every number quoted from this table
elsewhere still stands. Compile time is the volatile column: this machine's GPU has returned
anywhere between 672 ms and 2 513 ms across runs, because the OpenCL driver caches kernels. Quote
steady state; treat a single compile figure as one sample of a noisy quantity.
| NPU | — | \- | \- | not measured: no OpenVINO-visible NPU on this machine |

![Steady-state inference per device: 11.8 ms on CPU, 115.8 ms on the NVIDIA GPU, and an empty row
for the Intel NPU](charts/device-latency.svg)

The GPU row is not a disappointing result for Intel iGPUs; it is a *different device* — an NVIDIA
dGPU that OpenVINO reaches through the OpenCL ICD loader, advertising no FP16. It is in the table
because leaving it out would hide why `AUTO` resolves to something 10× slower than CPU here, which
is a trap for anyone reproducing M2b on this machine. See `docs/npu-compat.md`.

### Per-device inference on the Intel machine — measured 2026-08-09

Same tool, on the hardware the project is about. HerBERT INT8 sequence 128, Level Zero blob cache
cleared first. Raw report: `docs/doctor-intel-lunarlake.json`.

| Device | Full name | Compile | First inference | Steady |
|---|---|---|---|---|
| CPU | Intel Core Ultra 7 258V | 905 ms | 26.3 ms | **23.6 ms** |
| GPU | Intel Arc 140V (iGPU) | 913 ms | 5.9 ms | **2.7 ms** |
| NPU | Intel AI Boost (arch 4000) | 1 879 ms | 17.7 ms | **5.9 ms** |

![Steady-state inference on the Intel machine: 5.9 ms on the NPU, 2.7 ms on the Arc 140V iGPU and
23.6 ms on the CPU](charts/device-latency-intel.svg)

The full characterization, both shape variants and the energy comparison that is the point of the
exercise, is under [Device characterization (B4)](#device-characterization-b4--closed-2026-08-09).

#### The diagnostic reported a false negative on the NPU, and nearly published it

On the first run of this session `--doctor` printed `compiles: NO` for the NPU on **both** IR
variants, with Level Zero reporting `ZE_RESULT_ERROR_DEVICE_LOST — device hung, reset, was removed,
or driver update occurred`. Read at face value that is the project's central negative result: the
NPU refuses the model.

It was wrong, and what contradicted it was the harness sitting next to it. `sentin-bench --device
NPU` on the same IR reported `device used: NPU` and +3.85 ms, and the audit log settled it — 150
`person`, 150 `location` and 150 `organization` findings tagged `device: NPU`, the same counts as
the CPU and GPU runs. The model was executing on the NPU the whole time.

The fault was in the probe, not the device. It allocated one tensor per declared input and fed them
straight to the graph, with a comment saying it was feeding zeros — but `Tensor::new` only calls
`ov_tensor_create`, which **allocates without initialising**. The buffers held whatever was in that
memory, which as token ids are values around 1.4 × 10¹⁴ against a 50 k vocabulary, so the embedding
gather indexed far out of bounds. The regression test's failure output shows what was actually
being sent: `input_ids` full of heap pointer values. CPU and GPU absorb that silently; the NPU
hangs, and the driver reports it as device loss.

Three things are worth carrying out of this:

- **A device that "refuses the model" and a device handed invalid input are indistinguishable in
  the error.** `ZE_RESULT_ERROR_DEVICE_LOST` names no cause. Before recording an NPU refusal,
  check that the inputs were written on purpose.
- **Uninitialised memory is a device-dependent bug.** Fresh pages are often zero, which is why this
  survived every CPU and GPU run on the dev machine, and why it needed the one piece of hardware
  the project has least access to in order to show itself.
- **The diagnostic is the artefact the community is asked to attach to `npu-report` issues.** A
  false negative here would not have been a private mistake; it would have been a bug report
  against Intel's driver for a defect that was ours.

Fixed by writing every probe input explicitly — zeros, and ones for `attention_mask`, since an
all-zero mask is a degenerate input real inference never produces. Guarded by
`gateway/crates/sentin-detect/tests/probe_inputs.rs`, which asserts every byte reaching the device
was written on purpose, and which was checked by reverting the fix and watching it fail.

## M6 — gateway resource use — measured 2026-08-08

Dev machine, release build, resident set read from `/proc/<pid>/status` at startup and again after
200 inspected requests.

| Configuration | RSS at start | RSS after 200 requests | CPU |
|---|---|---|---|
| Layer 1 only (no model) | 8 MB | **9 MB** | 0.1 % idle |
| Layer 1 + layer 2 (HerBERT INT8 seq128, CPU) | 503 MB | **506 MB** | ~400 % under load (4 cores) |

**PASS on the stated threshold** — 9 MB against a 50 MB budget without a model, and memory is flat
under load rather than growing, which is what the second column is there to show.

The figure worth noticing is the other one. **The INT8 model is 123 MB on disk but costs roughly
500 MB resident**, so a deployment plan sized from the file on disk would be wrong by a factor of
four. Some of that is OpenVINO's own working buffers and compiled-graph state rather than weights.
It is not a problem on a developer laptop; it is a real consideration for a fleet of 8 GB client
machines, and it is the sort of number that only shows up if somebody measures it.

Layer 1 alone remains genuinely small, which matters: an operator unwilling to spend half a
gigabyte can run the deterministic layer on its own and still catch every structured identifier.

## Layer 1 — deterministic detectors (M1, M7) — measured 2026-08-08

Conditions: dev machine (AMD Ryzen AI 7 350), Fedora Linux, rustc 1.96.0, release profile,
criterion 100 samples per case. Reproduce with `cargo bench -p sentin-detect`.

### M1 — throughput (threshold > 100 MB/s)

| Corpus | 1 KB | 100 KB |
|---|---|---|
| Prose, no identifiers | 1.22 GiB/s | 1.22 GiB/s |
| Prose with identifiers (~1 per 200 B) | 904 MiB/s | 934 MiB/s |
| Digit noise (every token a candidate) | 296 MiB/s | 308 MiB/s |

![Layer 1 throughput on 100 KB inputs: 1 249 MiB/s on clean prose, 934 with identifiers, 308 on
digit noise, against a 100 MB/s threshold](charts/layer1-throughput.svg)

**PASS — the worst case is ~3× the threshold.** Throughput is essentially independent of input
size, as a single-pass scanner should be.

The digit-noise case is the honest worst case: every token is a candidate, so every token pays for
digit collection and at least one checksum. Real traffic looks like the first two rows.

One measurement worth recording because it was a 4× swing: an early version of the IBAN scanner
attempted a match at every alphabetic token start, which dropped clean-prose throughput to
335 MiB/s. Requiring the canonical `XX00` opening (uppercase country code, two check digits)
before scanning rejects ordinary words on the first byte and took it to 1.22 GiB/s.

### M7 — false positives (threshold 0 for checksum detectors)

| Corpus | Size | Checksum-backed findings |
|---|---|---|
| PII-free business prose (dates, prices, quantities, times, versions) | 1 MB | **0** |
| Text with no digits at all | 25 KB | 0 |
| Adversarial: uniform random 9-19 digit runs | 20 000 numbers | **710 (3.55 %)** |

**PASS on the corpus M7 is defined against**, and both cases are kept as tests
(`tests/false_positives.rs`) so a regression fails the build rather than a review.

**The 3.55 % on random digit runs is not a bug and will not go to zero.** REGON and NIP are mod-11
checks, so roughly one random nine- or ten-digit string in eleven satisfies them arithmetically.
The guards that exist — PESEL must also parse as a real date, cards need a known issuer prefix on
top of Luhn, and every candidate must sit on a token boundary — bring a naive rate of well over
10 % down to 3.55 %, but a checksum cannot distinguish a valid REGON from a random number that
happens to satisfy the same equation. This is the ceiling of what layer 1 can promise, and it is
why the architecture treats layer 1 as *evidence for* a decision rather than proof.

Practical consequence: a document consisting mostly of bare nine-digit numbers will produce
spurious REGON findings. Ordinary prose will not.

### Behaviour worth knowing

- Spans are **byte** offsets. All layer-1 patterns are ASCII, so they always land on character
  boundaries even in Polish text; layer 2 will have to convert the tokenizer's character offsets.
- Email and Polish phone numbers carry `Validation::Pattern` and **cannot reach a blocking
  decision** — there is no checksum to appeal to. This is enforced in the type system
  (`sentin_core::Finding::max_decision`), not by configuration.
- A bare nine-digit run is offered to REGON, not to the phone detector. Phone numbers are only
  recognised with a `+48`/`0048` prefix or `123 456 789` grouping, because guessing "phone" from
  an unformatted nine-digit number would fire on every order id in every prompt.

## Model selection (B1) — decided 2026-08-08

**Decision: `pczarnik/herbert-base-ner`.** It wins on every axis measured, including the one it
was expected to lose.

Conditions: dev machine (AMD Ryzen AI 7 350), device **CPU**, OpenVINO 2026.3.0, optimum-intel
2.1.0, transformers 5.5.4. Evaluation set: **WikiANN `test[:500]` per language**, exact span match,
scored on the PER/ORG/LOC intersection only (XLM-R also predicts DATE, which is excluded so the
comparison measures quality rather than class count).

| Model | Licence | seq | Precision | F1 PL | F1 EN | Size MB | `tokenizer.json` |
|---|---|---|---|---|---|---|---|
| `pczarnik/herbert-base-ner` | CC-BY-4.0 | 128 | FP32 | **88.06** | 58.97 | 476.4 | yes |
| | | 128 | INT8 | **87.57** | 59.51 | **123.4** | |
| | | 512 | FP32 | 88.06 | 62.86 | 476.4 | |
| | | 512 | INT8 | 87.16 | 63.74 | 123.4 | |
| `Davlan/xlm-roberta-base-ner-hrl` | AFL-3.0 | 128 | FP32 | 64.30 | 53.14 | 1075.1 | no |
| | | 128 | INT8 | 62.62 | 53.36 | 283.5 | |

![F1 by model and precision: HerBERT scores 88.06 Polish and 58.97 English at FP32, 87.57 and
59.51 at INT8; XLM-R scores 62.62 and 53.36 at INT8](charts/model-quality.svg)

### M3 — INT8 quality degradation (threshold ΔF1 < 2 pp)

| Model | seq | ΔF1 PL | ΔF1 EN | Verdict |
|---|---|---|---|---|
| herbert | 128 | −0.49 | +0.54 | PASS |
| herbert | 512 | −0.90 | +0.88 | PASS |
| xlmr | 128 | −1.68 | +0.22 | PASS |

(The seq-128 INT8 row above carried pre-reshape-fix figures for a while — 87.92 / 59.77 — which
contradicted these deltas. Re-measured 2026-08-08 on the shipped IR: 87.57 / 59.51, matching them.)

INT8 costs essentially nothing in quality while cutting the model to **~26 % of FP32 size**. Both
candidates quantize cleanly with full PTQ (weights *and* activations, calibrated on 300 WikiANN
sentences split evenly between PL and EN) — the `--weights-only` fallback was not needed.

### Why HerBERT won

- **Polish: +23.8 pp** (88.06 vs 64.30). Expected — XLM-R's `hrl` fine-tune set does not include
  Polish, so its Polish comes only from zero-shot transfer. B1 confirmed this rather than assuming it.
- **English: +5.8 pp** (58.97 vs 53.14), which was *not* expected — a Polish-only model beating a
  multilingual one on English. Both are mediocre here; see the limitation below.
- **Size: 2.3× smaller** (123 MB vs 284 MB at INT8). XLM-R carries a 250k-token vocabulary, so its
  embedding table dominates.
- **Phase 4 cost:** HerBERT ships `tokenizer.json`, loadable directly by the Rust `tokenizers`
  crate. XLM-R ships only `sentencepiece.bpe.model`, whose conversion additionally needs
  `protobuf` + `tiktoken` in the toolchain and would need converting for Rust.

### Limitations — read before quoting these numbers

- **English quality is weak (~59-64 F1) and this is a real limitation, not a measurement artefact.**
  The gateway's English NER will miss entities. M4 must report this honestly; improving it is a
  roadmap item, not a PoC deliverable. Layer 1 catches structured identifiers regardless of language.
- Absolute F1 here is lower than published WikiANN figures because scoring is **exact span match
  with no post-processing**, on a 500-sentence slice. The numbers are valid for *comparing* these
  candidates under identical conditions, which is what B1 needed; they are not a leaderboard entry.
- The synthetic fixtures in `tests/fixtures/` give much higher numbers (herbert FP32: 95.52 PL /
  91.30 EN) because the sentences are short and unambiguous. They exist for offline sanity checks,
  **not** for quality claims — with ~21 English gold entities, one miss moves F1 by ~5 pp, which is
  why the first M3 verdict taken from them showed a spurious 2.41 pp "failure". The M4 section
  below scores the Rust path over that same set, for the same reason: parity, not quality.
- NPU operator compatibility is **not** part of this decision — it cannot be tested on this machine.
  Phase 5 verifies it on Intel hardware; if HerBERT fails to compile for NPU, XLM-R is the fallback
  and these numbers say what that fallback costs.
- spaCy `pl_core_news_lg` was deliberately skipped; it returns in Phase 8 as article material.

## M4 — NER quality from the shipping path — measured 2026-08-08

Everything above was scored by `tools/validate_model.py`. That is the Python toolchain, and the
Python toolchain does not ship. What ships is `sentin-detect::ner`, which loads the same IR and
decodes the same logits with its own implementation of the same rules — tokenization, subword
aggregation, BIO merging, offset mapping. Two implementations are two chances to get those rules
wrong, and the ways they diverge are quiet: a span short by one subword, a label taken from the
wrong token, byte offsets treated as character offsets. None of them crash. They cost F1 that
nobody notices, in the component that decides whether an identifier is masked.

So M4 is measured from Rust, against the Python run as the reference.

Conditions: dev machine (AMD Ryzen AI 7 350), device **CPU**, OpenVINO 2026.3.0, HerBERT INT8
seq 128, exact span match on the PER/ORG/LOC intersection — identical scoring to B1. Evaluation
set: the committed fixtures, `tests/fixtures/ner_{pl,en}.jsonl` (36 PL / 24 EN sentences, 34 PL /
21 EN gold entities), gold spans derived from the `[TYPE:surface]` markup rather than maintained
by hand.

| Language | Precision | Recall | F1 (Rust) | F1 (Python reference) | Δ |
|---|---|---|---|---|---|
| PL | 96.97 | 94.12 | **95.52** | 95.52 | 0.00 |
| EN | 79.17 | 90.48 | **84.44** | 84.44 | 0.00 |

The two paths agree to two decimal places. They are not merely close: they decode the same logits
into the same spans, on Polish text where byte and character offsets are out of step in every
sentence. That is the property worth having, and `gateway/crates/sentin-detect/tests/ner_quality.rs`
pins it — if either side drifts the test fails and names the three usual causes rather than leaving
the next person to rediscover them.

**Which numbers to quote, and for what.** These fixture figures are far higher than the WikiANN
results in B1 (87.57 PL / 59.51 EN at the same precision and sequence length) because the fixtures
are short, unambiguous sentences written for offline sanity checking — no nested entities, no
ambiguous capitalisation, no rare surnames. A reader who took 95.52 as this model's Polish quality
would be badly misled.

- For **model quality**, quote B1's WikiANN numbers. They are the honest ones.
- For **implementation parity** — does the shipped engine reproduce the reference — quote these.

Two further caveats:

- The set is small enough that one entity moves English F1 by roughly 2 pp. It is a regression
  guard, not a quality measurement.
- English **precision (79.17) sits well below recall (90.48)**: the engine over-predicts on
  English, finding almost everything and inventing some. That is consistent with B1's finding that
  English is this model's weak side, and it is the direction of error a masking gateway can live
  with — an entity masked in error costs a word, an entity missed costs the identifier.

Reproduce (the test skips with a printed reason if the IR or the OpenVINO libraries are absent):

```bash
LD_LIBRARY_PATH=$OVLIB cargo test --release --manifest-path gateway/Cargo.toml \
    -p sentin-detect --test ner_quality -- --nocapture
```

### Does the NPU detect the same things as the CPU? — measured 2026-08-09

Everything above was scored with `device = CPU`. That leaves a hole under the project's central
claim: the NPU advertises `FP16 INT8` and a plugin is free to compute in FP16 internally, so
"the model runs on the NPU" is not the same statement as "the model detects the same entities on
the NPU". Identical timings would not reveal a difference; only scoring on the device does.

Both rows below are the **same IR** on the **same machine**, WikiANN, 500 sentences per language
(745 PL / 761 EN gold entities), only the device changed.

| Device | P (pl) | R (pl) | **F1 (pl)** | P (en) | R (en) | **F1 (en)** |
|---|---|---|---|---|---|---|
| CPU | 87.05 | 88.46 | **87.75** | 58.34 | 61.10 | **59.69** |
| NPU | 87.42 | 88.59 | **88.00** | 58.64 | 61.10 | **59.85** |

**The NPU is not bit-identical to the CPU, and the difference is immaterial.** ΔF1 is +0.25 pp
Polish and +0.16 pp English — on 745 and 761 gold entities that is one to three entities, and it
falls on the precision side (English recall is identical to the second decimal). The direction is
in the NPU's favour in this run, which is a coincidence of rounding rather than a finding: what
matters is the magnitude, and it is the size of FP16 rounding flipping a couple of borderline
tokens. There is no quality reason to prefer either device.

#### The published model is not the model these figures were measured on

Running the same command on this machine's copy exposed something worth stating plainly. The
released IR scores **87.75 / 59.69**; the locally generated IR scores **87.57 / 59.51** — the
figures quoted in B1 throughout this document. Same code, same corpus, same toolchain versions
(checked by pinning `transformers` and `numpy` to the lockfile and re-running: unchanged). The
artefacts simply differ:

| IR | sha256 of `openvino_model.bin` (first 16) | F1 pl / en |
|---|---|---|
| generated locally | `b07025a43698833e` | 87.57 / 59.51 |
| downloaded from the v0.0.0.5 release | `aaa3dc0fd500d790` | 87.75 / 59.69 |

**INT8 quantization is not reproducible.** NNCF calibrates over sampled data, CI regenerates the IR
from scratch on every tag, and the result is a different set of weights each time — worth about
±0.2 pp of F1 here. Two consequences:

- The B1 numbers describe *an* INT8 quantization of HerBERT, not *the* one you download. Treat
  ±0.2 pp as the reproducibility floor for any figure in this document that came from a quantized
  model.
- `SHA256SUMS.txt` verifies a download, never a rebuild — already noted for the archives, and true
  of the weights inside them for the same underlying reason.

## Energy (M5, M5b) — methodology fixed 2026-08-08, results pending hardware

Two different questions, deliberately kept apart:

- **M5** — how much power each *inference device* draws (NPU vs iGPU vs CPU) running the NER
  model. This is the article's headline comparison and needs an Intel Core Ultra machine.
- **M5b** — how much energy the *gateway itself* costs by sitting in the request path. Independent
  of which device runs inference, measurable on any machine, and the number an operator asks for
  when deciding whether to deploy this on a fleet of laptops.

### Running it

```bash
cargo run --release -p sentin-proxy --bin sentin-bench -- --energy
cargo run --release -p sentin-proxy --bin sentin-bench -- --energy --rps 1 --duration 60
```

Three phases of equal length — idle, direct-to-upstream, via-gateway — at a **fixed request rate**
rather than at saturation. A saturation test measures how fast the machine can spin; deployments
run at some rate and want the cost of that rate. Idle is subtracted from both workloads, and the
reported overhead is the *difference between the two workload phases*, so the mock upstream, the
HTTP client and the OS cancel out.

### What RAPL can and cannot tell you

| | |
|---|---|
| Interface | Linux powercap, `/sys/class/powercap/intel-rapl:*/energy_uj` |
| Works on | Intel **and** AMD — the `intel_rapl_msr` driver name is historical |
| Domains on the dev machine (2026-08-08) | `package-0`, `core` — **no `psys`, no NPU domain** |
| Counter behaviour | cumulative, wraps at `max_energy_range_uj` (65 532 610 987 µJ here); the reader handles wraparound, and there is a unit test for it |

**The caveat that governs every energy claim in this project: RAPL is package-scoped, and the NPU
does not get its own powercap domain.** You can measure what the whole SoC drew while a workload
ran; you cannot read "NPU watts". NPU energy has to be obtained by *differencing* — the same
workload at `device=NPU` and at `device=CPU`, subtracted — and attributed. Any figure presented as
NPU power that came straight out of a RAPL domain is actually package power. The harness prints
the domains it found so each report records what was really available on that machine rather than
assuming it generalises across Core Ultra generations.

### Permissions

Since the PLATYPUS side-channel disclosure, `energy_uj` is root-readable only, so the harness
cannot run unprivileged out of the box. It **refuses to start and prints the fix** rather than
silently reporting zeros:

```bash
sudo chmod a+r /sys/class/powercap/intel-rapl:*/energy_uj          # until reboot
# or persistently, as root:
echo 'SUBSYSTEM=="powercap", ACTION=="add", RUN+="/bin/chmod a+r /sys%p/energy_uj"' \
  > /etc/udev/rules.d/99-rapl-readable.rules
```

Windows has no RAPL sysfs. Intel PCM or an HWiNFO CSV log is the fallback; the two are not
interchangeable, so the method belongs in the result.

### Running M5b on another machine

**Reports are per-machine, not merged.** The question M5b answers is "what does the gateway cost on
*this* hardware" — an Intel machine reports its own overhead, an AMD machine reports its own. There
is no cross-architecture comparison table, because comparing gateway overhead between different
silicon says more about the two CPUs than about the gateway, and neither number transfers to a
third machine anyway.

What *does* travel with each result is the machine fingerprint, so a number is never orphaned from
the conditions that produced it:

```json
{ "cpu_model": "...", "os": "...", "kernel": "...",
  "cpu_governor": "performance", "platform_profile": "performance",
  "on_ac_power": true, "energy_backend": "powercap-rapl",
  "energy_domains": ["core", "package-0"] }
```

Before measuring, the harness checks that fingerprint and **warns rather than silently producing an
incomparable number**. Governor, ACPI profile and AC/battery each move the result enough to matter,
and six months later nobody remembers how a given run was configured.

Procedure on the Intel Core Ultra:

```bash
# 1. Rust, if not already present (the harness is built from source; a portable
#    binary is Phase 7's job -- see the blocker note below)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. Unlock the counters (same PLATYPUS restriction as everywhere else)
sudo chmod a+r /sys/class/powercap/intel-rapl:*/energy_uj

# 3. Pin the machine so the run is repeatable
sudo cpupower frequency-set -g performance
echo performance | sudo tee /sys/firmware/acpi/platform_profile     # if present
#    leave it on mains

# 4. Measure, into that machine's own report
cargo run --release -p sentin-proxy --bin sentin-bench -- \
    --energy --rps 10 --duration 60 --json docs/energy-intel-linux.json
```

Use the **Linux** installation on the Intel machine for energy work. Windows has no powercap sysfs;
RAPL there is reachable only through a signed kernel driver (Intel PCM), which is a different
backend with its own sampling, and its numbers must not be put in a column next to RAPL ones.
Windows stays the platform for functional verification, as Phase 5 already plans.

**Known blocker for a portable binary (Phase 7):** a static `x86_64-unknown-linux-musl` build fails
because `aws-lc-sys`, the default rustls crypto provider, needs a C toolchain for musl. Either
install `musl-gcc` on the build host or switch reqwest to `rustls-no-provider` and install the ring
provider explicitly. Until that is resolved the test machine needs a Rust toolchain.

### M5b results — per machine

#### dev machine — AMD Ryzen AI 7 350, Fedora (2026-08-08)

Backend `powercap-rapl`, saturation load, 20 s per phase, phases interleaved
(idle, direct, gateway, direct, gateway, idle). **Governor `powersave`, ACPI profile `balanced`** —
the harness flagged both; the figures are valid for that configuration and would move under
`performance`.

| Domain | Idle (W) | Noise floor (W) | Direct (W) | Gateway (W) | Direct (mJ/req) | Gateway (mJ/req) | **Overhead** |
|---|---|---|---|---|---|---|---|
| `package-0` | 6.55 | 0.12 | 24.61 | 22.93 | 0.528 | 1.098 | **+0.570 mJ/req** |
| `core` | 0.21 | 0.04 | 2.54 | 2.34 | 0.068 | 0.142 | **+0.075 mJ/req** |

Requests completed: 1 367 980 direct, 596 998 through the gateway.

**In practical terms:** at a sustained 10 requests per second the gateway adds about **5.7 mW**.
A million inspected requests cost roughly **0.16 Wh** — under half a percent of a typical laptop
battery.

Two traps this measurement walked into first, both worth repeating because they invert the
conclusion rather than merely blurring it:

1. **A single idle phase measured first is not idle.** It catches the machine cooling from
   whatever ran before. The first attempt reported idle at 9.61 W and the *workload* phases at
   7.19 W — the load apparently using less than nothing — and the idle-subtraction then clamped
   the overhead to a tidy, meaningless `0.000 mJ/req`. Idle is now measured at both ends and the
   drift between the two runs is reported as the noise floor.
2. **Comparing watts is wrong under saturation.** The gateway is the bottleneck, so it completes
   less than half the requests and therefore draws *less* power while costing *more* per request.
   Reading the power column alone would have concluded the gateway saves energy. The metric is
   energy per request, idle-subtracted, and the request counts are printed alongside it.

At 10 rps the signal sits below the platform's own drift and cannot be resolved at all — which is
itself the useful finding, and is reported as "below noise" rather than as zero. Saturation is
what makes the per-request cost measurable; the 10 rps figure above is derived from it.

#### Other machines

| Machine | Backend | Status |
|---|---|---|
| Intel Core Ultra 7 258V, Ubuntu 26.04 | powercap-rapl | **measured 2026-08-09** — +0.4120 mJ/req package; see below |
| Intel Core Ultra, Windows 11 | Intel PCM | pending; separate backend, never in the same column as RAPL |

### M5 results — per inference device (B4) — measured 2026-08-09

**Intel Core Ultra 7 258V (Lunar Lake), Ubuntu 26.04 LTS**, kernel 7.0.0-29-generic, OpenVINO
2026.3.0-22451, `intel_vpu` 1.0.0, NPU user space `intel-level-zero-npu` 1.33.0.20260529.
Governor `powersave`, on mains. HerBERT INT8 sequence 128, 20 s per device, idle subtracted.
Raw report: `docs/doctor-intel-lunarlake.json`.

**Five measured repeats per row plus a discarded warm-up, 15 s each**, at three load levels. Idle
is re-sampled every round: six samples, median 2.22 W, spread **0.35 W — the noise floor**. Raw
report including every individual repeat: `docs/power-intel-lunarlake.json`.

Repeats are not decorum here. A first single-pass run put NPU and iGPU 3.7 % apart, which is the
same order as the platform's own drift, and one measurement cannot tell "the NPU is cheaper" from
"the machine was quieter that minute". With five it can — see the trial ranges below.

| Device | Load | Throughput | Package W | Active W | **Energy per inference** | p95 |
|---|---|---|---|---|---|---|
| CPU | saturation | 40.1 /s | 24.41 | 22.19 | **554.60 mJ** | 557.53 |
| GPU — Arc 140V | saturation | 375.4 /s | 20.79 | 18.54 | **49.51 mJ** | 49.99 |
| NPU — AI Boost | saturation | 299.0 /s | **16.16** | **13.96** | **46.81 mJ** | 47.66 |
| CPU | 10 rps | 9.6 /s | 9.00 | 6.92 | 724.14 mJ | 736.45 |
| GPU — Arc 140V | 10 rps | 9.4 /s | 3.66 | 1.53 | 160.08 mJ | 182.12 |
| NPU — AI Boost | 10 rps | 9.9 /s | **2.99** | **0.78** | **78.21 mJ** | 102.74 |
| CPU | 1 rps | 0.9 /s | 3.41 | 1.19 | 1 267.08 mJ | 1 503.74 |
| GPU — Arc 140V | 1 rps | 1.0 /s | 2.98 | 0.78 | 815.77 mJ | 1 002.87 |
| NPU — AI Boost | 1 rps | 1.0 /s | 2.37 | 0.17 | *below noise* | — |

![Energy per inference at 10 rps: 724.14 mJ on CPU, 160.08 mJ on the Intel iGPU and 78.21 mJ on the
NPU](charts/device-energy.svg)

**At saturation the accelerators are close; at a realistic load they are not.** That is the finding,
and it is the opposite of what a single saturation run suggested:

| Load | NPU vs iGPU | NPU vs CPU |
|---|---|---|
| saturation | **1.06× cheaper** | 11.8× |
| 10 rps | **2.05× cheaper** | 9.3× |

At saturation both accelerators amortise their fixed costs over as much work as possible and the
gap narrows to 5.5 %. At ten requests per second — a plausible load for a gateway in front of a
few agents — the iGPU still pays to be powered up and clocked while the NPU does not, and the
NPU comes out **twice as cheap per inference**. A gateway inspecting traffic in the background
lives at the second row, not the first.

**The trial ranges do not overlap**, which is what makes the saturation comparison sayable at all:

| Device | Five repeats, mJ per inference |
|---|---|
| NPU | 45.74 · 46.51 · 46.81 · 47.66 · 47.66 |
| GPU | 49.15 · 49.43 · 49.51 · 49.85 · 49.99 |
| CPU | 548.44 · 553.16 · 554.60 · 556.80 · 557.53 |

The NPU's worst repeat is cheaper than the iGPU's best. With one sample each that separation could
not have been claimed.

![Energy per inference at saturation: 46.81 mJ on the NPU, 49.51 on the Intel iGPU and 554.60 on
the CPU, with non-overlapping repeat ranges](charts/device-energy-saturation.svg)

**One row is marked *below noise* and that is a result, not a gap.** At 1 rps the NPU's own draw is
0.17 W against a 0.35 W noise floor — an inference costing tens of millijoules spread over a whole
second is tens of milliwatts, and package RAPL on a laptop cannot separate that from the platform's
drift. The harness says so on the row rather than printing a number that looks like a measurement.
The honest reading is "at one request per second the NPU's inference is invisible in package
power", which is itself worth knowing.

Per the caveat above, these are package readings *while* a device was working. There is no NPU
powercap domain, so the NPU's own draw is the distance between its row and the CPU row, not an
absolute figure.

Reproduce with:

```bash
sentin-doctor --model models/seq128/openvino_model.xml \
    --power --power-seconds 15 --power-repeats 5 --power-json power.json
```

### M5b on this machine — gateway overhead, Intel (2026-08-09)

Same harness and interleaving as the dev-machine run, saturation, 30 s phases, `powersave`
governor (flagged by the harness as a comparability warning).

| Domain | Idle (W) | Noise (W) | Direct (W) | Gateway (W) | mJ/req direct | mJ/req gateway | Overhead |
|---|---|---|---|---|---|---|---|
| core | 0.63 | 0.06 | 8.57 | 7.58 | 0.2904 | 0.6681 | **+0.3777 mJ/req** |
| dram | 0.16 | 0.00 | 0.20 | 0.21 | 0.0015 | 0.0052 | +0.0038 mJ/req |
| package-0 | 2.34 | 0.06 | 11.21 | 10.00 | 0.3245 | 0.7364 | **+0.4120 mJ/req** |

Requests completed: direct 1 641 132, gateway 624 078 — different by design at saturation, which is
why the comparison is per request rather than in total.

**This does not go in the same column as the dev machine's +0.570 mJ/req.** Per the rule above,
M5b results are per machine and are never merged: the two numbers describe two CPUs under two
governors, not two versions of the gateway.

## Device characterization (B4) — closed 2026-08-09

Measured on one Intel Core Ultra 7 258V, all three devices on the same physical machine, as the
method requires. HerBERT INT8; both IR variants tried.

**Both shape variants compile and execute on the NPU.** The backup model from B1 was not needed.

| Device | seq128 compile | seq128 steady | seq512 compile | seq512 steady | M2b added p95 (seq128) |
|---|---|---|---|---|---|
| NPU — Intel AI Boost (arch 4000) | 1 878.9 ms | 5.9 ms | 6 090.0 ms | 20.7 ms | **+3.85 ms** (budget 80) |
| GPU — Arc 140V iGPU | 912.6 ms | **2.7 ms** | 995.5 ms | **9.7 ms** | +3.09 ms (budget 150) |
| CPU — Core Ultra 7 258V | 905.1 ms | 23.6 ms | 661.4 ms | 108.4 ms | +24.97 ms (budget 150) |

Every device passes M2b with large margin; the seq512 variant on NPU adds +12.75 ms p95, also a
pass. Latency is not what separates these devices — energy is, and that is the table above.

**NPU compile time is a first-run cost, not a per-start cost.** The figures above are with the
Level Zero blob cache cleared. Left in place, the driver's cache (`~/.cache/ze_intel_npu_cache`,
329 MB after both variants) brings NPU compilation down to **37.8 ms for seq128 and 60.4 ms for
seq512** — fifty- to hundred-fold. So a cold first start pays six seconds for seq512 and every
start after that pays sixty milliseconds. Packaging should expect that cache directory to exist and
grow; a deployment that wipes it between restarts re-pays the full compile each time.

**No operator falls back.** A model that compiles can still be split across devices, and that split
is invisible in a latency figure — so it was checked rather than assumed. `openvino` 0.11 exposes
neither `query_model` nor compiled-model properties, so this comes from the Python side:

```bash
tools/.venv/bin/python tools/query_ops.py --model herbert --seq 128 --json ops.json
```

| Variant | Operations | Distinct types | NPU claims | GPU claims | CPU claims |
|---|---|---|---|---|---|
| seq128 | 1 467 | 25 | **1 467 — none unclaimed** | 1 467 | 1 467 |
| seq512 | 1 467 | 25 | **1 467 — none unclaimed** | 1 467 | 1 467 |

Every one of the 1 467 nodes is accepted by the NPU plugin in both variants, so the timings above
describe the NPU running the whole graph, not the NPU running most of it while the CPU quietly
finishes the rest.

Note: the dev machine's `GPU` device is an NVIDIA dGPU reached through the OpenCL ICD, not an
Intel iGPU — it does not belong in this table. See `docs/npu-compat.md`.
