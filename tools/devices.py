# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
"""Report the OpenVINO device inventory of this machine.

This is what produces the tables in ``docs/npu-compat.md``. It deliberately goes past
``available_devices``: enumeration proves nothing, so each device is also asked to compile and
execute a tiny model. A device that lists but cannot run is worse than one that is absent, and
that distinction is exactly what community NPU reports need to capture.

Usage::

    tools/.venv/bin/python tools/devices.py
    tools/.venv/bin/python tools/devices.py --json
"""

from __future__ import annotations

import argparse
import contextlib
import json
import platform
from typing import Any

import numpy as np
import openvino as ov
import openvino.opset15 as ops

# Properties worth reporting per device. Missing ones are skipped: the set varies by plugin.
_PROPERTIES = (
    "FULL_DEVICE_NAME",
    "DEVICE_TYPE",
    "DEVICE_ARCHITECTURE",
    "GPU_DEVICE_ID",
    "DEVICE_UUID",
    "OPTIMIZATION_CAPABILITIES",
)


def _tiny_model() -> ov.Model:
    """A matmul+ReLU small enough to compile anywhere, big enough to prove execution."""
    rng = np.random.default_rng(0)
    param = ops.parameter([1, 64], np.float32, name="x")
    weights = ops.constant(rng.random((64, 64)).astype(np.float32))
    return ov.Model([ops.relu(ops.matmul(param, weights, False, False))], [param], "tiny")


def _probe(core: ov.Core, device: str, model: ov.Model) -> dict[str, Any]:
    """Compile and run ``model`` on ``device``; never raises, reports the failure instead."""
    try:
        compiled = core.compile_model(model, device)
        result = compiled(np.ones((1, 64), dtype=np.float32))
        return {"executes": True, "output_sum": float(next(iter(result.values())).sum())}
    except Exception as exc:
        return {"executes": False, "error": f"{type(exc).__name__}: {exc}"}


def inventory() -> dict[str, Any]:
    """Collect versions, devices, properties and an execution probe per device."""
    core = ov.Core()
    model = _tiny_model()
    devices: dict[str, Any] = {}
    for name in core.available_devices:
        props: dict[str, Any] = {}
        for prop in _PROPERTIES:
            # Plugins legitimately lack properties; an absent one is not an error.
            with contextlib.suppress(Exception):
                props[prop] = str(core.get_property(name, prop))
        devices[name] = {"properties": props, "probe": _probe(core, name, model)}
    return {
        "openvino_version": ov.__version__,
        "platform": f"{platform.system()} {platform.release()} ({platform.machine()})",
        "python": platform.python_version(),
        "available_devices": core.available_devices,
        "devices": devices,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    args = parser.parse_args()

    data = inventory()
    if args.json:
        print(json.dumps(data, indent=2))
        return

    print(f"OpenVINO {data['openvino_version']}")
    print(f"Platform {data['platform']}, Python {data['python']}")
    print(f"available_devices: {data['available_devices']}\n")
    for name, info in data["devices"].items():
        print(f"=== {name}")
        for prop, value in info["properties"].items():
            print(f"    {prop:<26} {value}")
        probe = info["probe"]
        if probe["executes"]:
            print(f"    {'COMPILE + EXECUTE':<26} OK (output sum {probe['output_sum']:.3f})")
        else:
            print(f"    {'COMPILE + EXECUTE':<26} FAILED: {probe['error']}")
        print()
    if "NPU" not in data["available_devices"]:
        print("No NPU device. On Intel hardware this means a missing or mismatched NPU driver;")
        print("on this AMD dev machine it is expected — see docs/npu-compat.md.")


if __name__ == "__main__":
    main()
