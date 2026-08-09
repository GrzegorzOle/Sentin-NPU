#!/usr/bin/env python3
# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0

"""Report which operations a device's OpenVINO plugin will take, and which fall back.

This answers the one Phase 5 question the Rust side cannot: `openvino` 0.11 exposes neither
``query_model`` nor properties on a compiled model, so an operator-level breakdown has to come from
the Python bindings. ``--doctor`` says whether the graph compiles and how fast it runs; this says
*what the device agreed to run*, which is the difference between "the NPU executed the model" and
"the NPU executed most of the model and the CPU quietly did the rest".

A model that compiles can still be split across devices, and that split is invisible in a latency
number. Reporting zero fallback is only meaningful if it was actually checked.

Usage::

    tools/.venv/bin/python tools/query_ops.py --model herbert --seq 128
    tools/.venv/bin/python tools/query_ops.py --model-dir /path/to/ir --device NPU --json ops.json
"""

from __future__ import annotations

import argparse
import collections
import json
import sys
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[1]


def resolve_xml(args: argparse.Namespace) -> Path:
    """Locate the IR, either from an explicit directory or from the toolchain's layout."""
    if args.model_dir:
        return Path(args.model_dir) / "openvino_model.xml"
    return REPO / "models" / args.model / "int8" / f"seq{args.seq}" / "openvino_model.xml"


def query(xml: Path, devices: list[str]) -> dict[str, Any]:
    """Ask each device which nodes of the model it will take, and summarise what it will not.

    Returns a JSON-ready structure: total operation counts by type, and per device the number of
    claimed nodes plus a breakdown of the unclaimed ones.
    """
    import openvino as ov  # imported here so --help works without the wheel installed

    core = ov.Core()
    model = core.read_model(str(xml))
    ops = model.get_ops()
    by_type = collections.Counter(op.get_type_name() for op in ops)

    report: dict[str, Any] = {
        "model": str(xml),
        "openvino": ov.__version__,
        "available_devices": list(core.available_devices),
        "operations": len(ops),
        "distinct_types": len(by_type),
        "operations_by_type": dict(sorted(by_type.items())),
        "devices": [],
    }

    for device in devices:
        entry: dict[str, Any] = {"device": device}
        try:
            supported = set(core.query_model(model, device))
        # A device refusing to answer is a result worth recording, not a reason to stop.
        except Exception as err:
            entry["error"] = str(err)
            report["devices"].append(entry)
            continue

        unclaimed = [op for op in ops if op.get_friendly_name() not in supported]
        entry["claimed"] = len(supported)
        entry["unclaimed"] = len(unclaimed)
        entry["unclaimed_by_type"] = dict(
            sorted(collections.Counter(op.get_type_name() for op in unclaimed).items())
        )
        report["devices"].append(entry)
    return report


def main() -> int:
    """Print the per-device operator report, and optionally write it as JSON."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="herbert", help="model name under models/")
    parser.add_argument("--seq", type=int, default=128, choices=(128, 512))
    parser.add_argument("--model-dir", help="explicit IR directory, overriding --model/--seq")
    parser.add_argument(
        "--device",
        action="append",
        help="device to query; repeatable. Default: every device the machine exposes.",
    )
    parser.add_argument("--json", help="also write the report here")
    args = parser.parse_args()

    xml = resolve_xml(args)
    if not xml.exists():
        print(f"no IR at {xml} — run tools/prepare_model.py and tools/quantize.py", file=sys.stderr)
        return 1

    import openvino as ov

    devices = args.device or list(ov.Core().available_devices)
    report = query(xml, devices)

    print(f"{report['model']}")
    print(f"  OpenVINO {report['openvino']}, devices {report['available_devices']}")
    print(f"  {report['operations']} operations, {report['distinct_types']} distinct types\n")
    for entry in report["devices"]:
        if "error" in entry:
            print(f"  {entry['device']:<5} query_model failed: {entry['error']}")
            continue
        verdict = "no fallback" if entry["unclaimed"] == 0 else f"{entry['unclaimed']} unclaimed"
        print(
            f"  {entry['device']:<5} claims {entry['claimed']}/{report['operations']} — {verdict}"
        )
        for name, count in entry["unclaimed_by_type"].items():
            print(f"        {name}: {count}")

    if args.json:
        Path(args.json).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(f"\nwrote {args.json}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
