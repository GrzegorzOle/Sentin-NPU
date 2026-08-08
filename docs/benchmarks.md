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
| M3 | INT8 quality degradation | ΔF1 < 2 pp | not measured |
| M4 | NER quality PL+EN | reported, no hard threshold | not measured |
| M5 | Power draw per device | no threshold — **headline result** | not measured |
| M6 | Gateway resource use | RSS < 50 MB without model | not measured |
| M7 | L1 false positives | 0 for checksum detectors | not measured |

## Model selection (B1)

Phase 1 fills this in: XLM-RoBERTa NER vs HerBERT-based PL NER vs spaCy `pl_core_news_lg`
baseline, compared on PL quality, INT8 size, and NPU operator compatibility.

| Model | License | Params | INT8 size | F1 (PL) | F1 (EN) | NPU-compatible |
|---|---|---|---|---|---|---|
| — | | | | | | |

## Device characterization (B4)

Phase 5 fills this in, on one Intel Core Ultra machine.

| Device | Latency p50 (ms) | Latency p95 (ms) | Throughput (rps) | Power idle (W) | Power @1 rps | Power @10 rps |
|---|---|---|---|---|---|---|
| NPU | | | | | | |
| GPU (Intel iGPU) | | | | | | |
| CPU | | | | | | |

Note: the dev machine's `GPU` device is an NVIDIA dGPU reached through the OpenCL ICD, not an
Intel iGPU — it does not belong in this table. See `docs/npu-compat.md`.
