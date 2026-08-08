<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# Device compatibility matrix

This file is the running record of *which OpenVINO devices exist on which machine, and what they
actually do*. It is also the artifact community `npu-report` issues feed into.

Every entry states: machine, OS, OpenVINO version, driver versions, `available_devices`, and
whether a model **compiled and executed** — enumeration alone proves nothing.

---

## Dev machine (primary) — no Intel NPU

| | |
|---|---|
| OS | Fedora Linux (kernel 7.1.6) |
| CPU | AMD Ryzen AI 7 350 w/ Radeon 860M |
| NPU | AMD XDNA — present as `/dev/accel/accel0`, **not an OpenVINO device** |
| dGPU | NVIDIA GeForce RTX 5070 Laptop |
| OpenVINO | 2026.3.0-22451-8a17657b995 (project venv) — identical result on 2026.2.0 (system) |
| Python | 3.11.15 (`tools/.venv`) |
| Date | 2026-08-08 |

```
available_devices → ['CPU', 'GPU']
```

Reproduce with:

```bash
tools/.venv/bin/python tools/devices.py          # human-readable
tools/.venv/bin/python tools/devices.py --json   # for npu-report issues
```

### CPU

```
FULL_DEVICE_NAME          AMD Ryzen AI 7 350 w/ Radeon 860M
DEVICE_TYPE               INTEGRATED
DEVICE_ARCHITECTURE       intel64
OPTIMIZATION_CAPABILITIES BF16, WINOGRAD, FP32, INT8, BIN, EXPORT_IMPORT
```

Compile + execute: **OK**. This is the development target for Phases 1-4.

### GPU — resolves B0, and it is not what the plan assumed

The plan stated the NVIDIA GPU "is not an OpenVINO target (no CUDA support in OV)". On this
machine that is **wrong in practice**:

```
FULL_DEVICE_NAME          NVIDIA GeForce RTX 5070 Laptop GPU (dGPU)
DEVICE_TYPE               DISCRETE
DEVICE_ARCHITECTURE       GPU: vendor=0x10de arch=v12.0.0     ← 0x10de = NVIDIA PCI vendor ID
OPTIMIZATION_CAPABILITIES FP32, BIN, INT8, EXPORT_IMPORT       ← note: no FP16
```

**Why it appears:** OpenVINO's GPU plugin enumerates through the OpenCL ICD loader, and the only
ICDs installed here are `/etc/OpenCL/vendors/nvidia.icd` and `xilinx.icd`. It therefore picks up
NVIDIA's OpenCL driver. No CUDA is involved. The AMD Radeon 860M iGPU does **not** appear — no
Mesa/rusticl OpenCL ICD is installed.

**It executes.** A matmul+ReLU model compiled and ran on `GPU`, producing output matching CPU to
float tolerance (sum 2033.319 vs 2033.290). Reproduced on both OpenVINO 2026.2.0 and 2026.3.0.

**Caveats — do not over-read this result:**

- One trivial op pair proves the OpenCL path works, not that a transformer NER model will compile.
  Retest in Phase 4 with the real IR before relying on it.
- `FP16` is absent from the capability list, unlike on Intel GPUs. Anything assuming FP16 on GPU
  will fail here.
- This configuration is unsupported by Intel. It is fine as a *local convenience* and possibly
  interesting for the article, but it is **not** a substitute for the Intel iGPU numbers that B4
  requires, and NPU-vs-GPU-vs-CPU comparisons must still happen on one Intel machine (M5 rule).

**Decision (B0, closed 2026-08-08):** treat dev-machine `GPU` as an opportunistic extra target,
not a planned one. Device selection code must not assume `GPU` means Intel. Benchmarks published
from this machine are labelled CPU-only unless the GPU entry is re-verified with the real model.

### AMD XDNA NPU

Not visible to OpenVINO under any device name. Reaching it needs ONNX Runtime + Vitis AI EP —
out of PoC scope (roadmap: multi-vendor NPU).

---

## Intel test machines (secondary) — pending

Phase 5 fills these in. Required per machine: NPU driver version (Windows: Intel NPU driver
package; Linux: `intel-npu-driver` + Level Zero), `available_devices`, per-IR-variant (seq 128 /
512) compile result, list of operators that fell back to CPU, model compile time, first-inference
time.

| Machine | OS | OV | Driver | `available_devices` | IR-128 | IR-512 | Notes |
|---|---|---|---|---|---|---|---|
| Intel Core Ultra | Windows 11 | — | — | — | — | — | not yet run |
| Intel Core Ultra | Linux | — | — | — | — | — | not yet run |

---

## API notes (things that cost time)

- `openvino.runtime` **no longer exists** in 2026.x — it was removed, not just deprecated. Use
  `import openvino as ov` plus `import openvino.opsetNN as ops`. Training-era snippets that
  `from openvino.runtime import ...` fail immediately with `ModuleNotFoundError`.
