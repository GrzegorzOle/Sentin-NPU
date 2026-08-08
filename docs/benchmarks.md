<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# Benchmarks

> **Status: no measurements yet.** Phase 0 established the environment; M1 arrives with Phase 2.

Every result recorded here states: **date, commit, hardware, OpenVINO version, driver version**.
A number without that context is not a result.

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
| M2b | Full pipeline overhead (L1+L2) | p95 < 150 ms CPU / < 80 ms NPU | awaits L2 (Phase 4) |
| M2c | Streaming TTFT impact | decided by B2; always reported | **measured** — see B2 |
| M3 | INT8 quality degradation | ΔF1 < 2 pp | **PASS** — see B1 below |
| M4 | NER quality PL+EN | reported, no hard threshold | preliminary, see B1 |
| M5 | Power draw per device | no threshold — **headline result** | not measured |
| M6 | Gateway resource use | RSS < 50 MB without model | not measured |
| M7 | L1 false positives | 0 for checksum detectors | **PASS** on prose; see caveat below |

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

### M2c — streaming, and the decision for B2

The mock emits 40 SSE events 12 ms apart (~0.5 s of generation), ending a sentence every eighth
event — a short answer from a fast local model.

| Strategy | TTFT p50 (ms) | TTFT p95 (ms) | Total p50 (ms) | TTFT vs baseline |
|---|---|---|---|---|
| Direct to upstream (baseline) | 0.3 | 0.4 | 511.0 | — |
| Gateway, `passthrough` | 0.4 | 0.6 | 510.8 | **+0.1 ms** |
| Gateway, `sliding_window` | 92.5 | 93.2 | 511.3 | **+92 ms** |
| Gateway, `buffer` | 511.0 | 512.9 | 511.0 | **+511 ms** |

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

## Layer 1 — deterministic detectors (M1, M7) — measured 2026-08-08

Conditions: dev machine (AMD Ryzen AI 7 350), Fedora Linux, rustc 1.96.0, release profile,
criterion 100 samples per case. Reproduce with `cargo bench -p sentin-detect`.

### M1 — throughput (threshold > 100 MB/s)

| Corpus | 1 KB | 100 KB |
|---|---|---|
| Prose, no identifiers | 1.22 GiB/s | 1.22 GiB/s |
| Prose with identifiers (~1 per 200 B) | 904 MiB/s | 934 MiB/s |
| Digit noise (every token a candidate) | 296 MiB/s | 308 MiB/s |

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
| | | 128 | INT8 | **87.92** | 59.77 | **123.4** | |
| | | 512 | FP32 | 88.06 | 62.86 | 476.4 | |
| | | 512 | INT8 | 87.16 | 63.74 | 123.4 | |
| `Davlan/xlm-roberta-base-ner-hrl` | AFL-3.0 | 128 | FP32 | 64.30 | 53.14 | 1075.1 | no |
| | | 128 | INT8 | 62.62 | 53.36 | 283.5 | |

### M3 — INT8 quality degradation (threshold ΔF1 < 2 pp)

| Model | seq | ΔF1 PL | ΔF1 EN | Verdict |
|---|---|---|---|---|
| herbert | 128 | −0.14 | +0.80 | PASS |
| herbert | 512 | −0.90 | +0.88 | PASS |
| xlmr | 128 | −1.68 | +0.22 | PASS |

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
  why the first M3 verdict taken from them showed a spurious 2.41 pp "failure".
- NPU operator compatibility is **not** part of this decision — it cannot be tested on this machine.
  Phase 5 verifies it on Intel hardware; if HerBERT fails to compile for NPU, XLM-R is the fallback
  and these numbers say what that fallback costs.
- spaCy `pl_core_news_lg` was deliberately skipped; it returns in Phase 8 as article material.

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

| Machine | Backend | Governor / profile | Rate | Domain | Idle (W) | Direct (W) | Gateway (W) | Overhead (mJ/req) |
|---|---|---|---|---|---|---|---|---|
| dev — AMD Ryzen AI 7 350, Fedora | powercap-rapl | powersave / balanced | — | — | — | — | — | not run: `energy_uj` root-only here |
| Intel Core Ultra, Linux | powercap-rapl | — | — | — | — | — | — | pending |
| Intel Core Ultra, Windows 11 | Intel PCM | — | — | — | — | — | — | pending; separate backend, not comparable to the rows above |

### M5 results — per inference device (B4)

Phase 5 fills this in, on one Intel Core Ultra machine. NPU rows are differenced against the CPU
row, per the caveat above, not read from a domain.

## Device characterization (B4)

Phase 5 fills this in, on one Intel Core Ultra machine.

| Device | Latency p50 (ms) | Latency p95 (ms) | Throughput (rps) | Power idle (W) | Power @1 rps | Power @10 rps |
|---|---|---|---|---|---|---|
| NPU | | | | | | |
| GPU (Intel iGPU) | | | | | | |
| CPU | | | | | | |

Note: the dev machine's `GPU` device is an NVIDIA dGPU reached through the OpenCL ICD, not an
Intel iGPU — it does not belong in this table. See `docs/npu-compat.md`.
