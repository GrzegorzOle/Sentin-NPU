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
| M1 | L1 throughput (MB/s) | > 100 MB/s | not measured |
| M2a | Proxy overhead, no inspection | p95 < 5 ms | not measured |
| M2b | Full pipeline overhead (L1+L2) | p95 < 150 ms CPU / < 80 ms NPU | not measured |
| M2c | Streaming TTFT impact | decided by B2; always reported | not measured |
| M3 | INT8 quality degradation | ΔF1 < 2 pp | **PASS** — see B1 below |
| M4 | NER quality PL+EN | reported, no hard threshold | preliminary, see B1 |
| M5 | Power draw per device | no threshold — **headline result** | not measured |
| M6 | Gateway resource use | RSS < 50 MB without model | not measured |
| M7 | L1 false positives | 0 for checksum detectors | not measured |

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

## Device characterization (B4)

Phase 5 fills this in, on one Intel Core Ultra machine.

| Device | Latency p50 (ms) | Latency p95 (ms) | Throughput (rps) | Power idle (W) | Power @1 rps | Power @10 rps |
|---|---|---|---|---|---|---|
| NPU | | | | | | |
| GPU (Intel iGPU) | | | | | | |
| CPU | | | | | | |

Note: the dev machine's `GPU` device is an NVIDIA dGPU reached through the OpenCL ICD, not an
Intel iGPU — it does not belong in this table. See `docs/npu-compat.md`.
