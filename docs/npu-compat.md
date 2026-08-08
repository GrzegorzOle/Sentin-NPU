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

## Quantization silently destroys static shapes — check before blaming the NPU

**The single most important finding so far for anyone taking a quantized model to an NPU.**

`OVQuantizer.quantize()` returns a model whose inputs are **dynamic**, discarding a reshape applied
before it:

| Variant | Input shape |
|---|---|
| `fp32/seq128` | `[1,128]` — static, as exported |
| `int8/seq128` *(before the fix)* | `[?,?]` — **dynamic** |

Static shapes are an NPU requirement. Shipping the quantized model as produced would therefore
have made the NPU reject it, and the symptom would have read as *"the NPU cannot run our model"*
rather than *"our toolchain silently un-reshaped it"* — a wrong conclusion, drawn on scarce
hardware, and quite possibly reported upstream as an OpenVINO defect.

`tools/quantize.py` now re-applies the reshape after quantization and **verifies it took**, failing
loudly if the model is still dynamic.

Two further traps in the fix itself:

- **Never re-save an IR into the directory it was loaded from.** OpenVINO keeps the weights file
  mapped while the model is live, so saving in place truncates the `.bin` and leaves an IR that no
  longer parses at all — `Unable to read the model ... Available frontends: ir jax onnx ...`, which
  looks like a corrupt download rather than self-inflicted damage. Write to a sibling directory and
  swap.
- `save_pretrained` does not re-emit the tokenizer or `config.json`, so those must be copied across
  or the model becomes unusable to the scorer.

This was caught by `--doctor` on a machine with **no** NPU at all, which is the point of having it.

## Diagnostics: `sentin-gateway --doctor`

One command that reports what a machine can actually do, and the mechanism behind `npu-report`
issues. It compiles and executes the real IR on every enumerated device rather than reading
capability lists, because a device that enumerates may still refuse the model.

```bash
sentin-gateway --doctor \
  --model models/herbert/int8/seq128/openvino_model.xml \
  --json my-machine.json
```

Result on the dev machine (AMD, no NPU), OpenVINO 2026.3, HerBERT INT8 seq128:

| Device | Compiles | Compile (ms) | First infer (ms) | Steady (ms) |
|---|---|---|---|---|
| CPU — AMD Ryzen AI 7 350 | yes | 686 | 14.3 | **11.7** |
| GPU — NVIDIA RTX 5070 (via OpenCL ICD) | yes | 706 | 116.5 | 116.0 |

The NVIDIA path runs the full transformer correctly but roughly ten times slower than CPU, which
is what an unsupported configuration should be expected to look like. First-inference time is
reported separately from steady state because on an NPU the first call includes graph setup and
the difference is a deployment concern, not a rounding error.

Without the OpenVINO libraries the command still produces a machine report and says exactly what
is missing, rather than failing opaquely.

## Binding OpenVINO from Rust (verified 2026-08-08)

The gateway runtime is Rust, so it reaches OpenVINO through the [`openvino`](https://crates.io/crates/openvino)
crate. Verified against runtime **2026.3.0** on the dev machine:

```toml
openvino = { version = "0.11", features = ["runtime-linking"] }
```

| Check | Result |
|---|---|
| Loads the installed runtime | yes — reports `2026.3.0-22451-8a17657b995` |
| `available_devices` | `[CPU, GPU]`, identical to the Python API |
| `DeviceFullName`, `DeviceCapabilities` | read back correctly |
| Arbitrary property keys | `PropertyKey::Other(..)` works — plugin-specific properties (including NPU ones) are reachable without patching the crate |

**`runtime-linking` is the feature to use.** Without it the build script needs to find an OpenVINO
installation at compile time and fails with *"Unable to find an OpenVINO installation on your
system"*. With it, the libraries are loaded via `dlopen` at startup, which also means a release
archive can ship the binary and the runtime together without build-time coupling.

**Gotcha that costs an afternoon:** `dlopen` looks for *unversioned* sonames, and the OpenVINO
Python wheel ships only versioned ones (`libopenvino_c.so.2630`). The binary then fails at startup
with *"Unable to find the `openvino_c` library to load"* even though the library is plainly there.
Create unversioned symlinks alongside:

```bash
OVLIB=<...>/site-packages/openvino/libs
for f in "$OVLIB"/*.so.*; do
  base=$(basename "$f"); ln -sf "$f" "$OVLIB/${base%%.so.*}.so"
done
export LD_LIBRARY_PATH="$OVLIB:$LD_LIBRARY_PATH"
```

**The NPU plugin ships with the wheel.** `libopenvino_intel_npu_plugin.so` is present in the same
directory, so an Intel machine needs the NPU kernel driver but *not* a separate OpenVINO runtime
install. That simplifies both test-machine setup and release packaging.

## API notes (things that cost time)

- `openvino.runtime` **no longer exists** in 2026.x — it was removed, not just deprecated. Use
  `import openvino as ov` plus `import openvino.opsetNN as ops`. Training-era snippets that
  `from openvino.runtime import ...` fail immediately with `ModuleNotFoundError`.
