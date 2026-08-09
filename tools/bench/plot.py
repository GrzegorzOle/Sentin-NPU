# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0

"""Render the measured benchmarks as SVG charts for ``docs/benchmarks.md`` and the article.

Why this writes SVG by hand instead of calling matplotlib: the Python side of this project is an
offline model toolchain, and its dependency list is something a reader is asked to install before
they can reproduce anything. A plotting stack is a lot of weight to add for five charts that are
bars on a linear axis. The output is also plain text, so a chart change shows up in review as a
diff rather than as a new binary blob.

**The numbers here are copied from ``docs/benchmarks.md``, which stays the source of truth.** They
are duplicated deliberately rather than parsed out of the Markdown: a parser over prose breaks
silently when the prose is edited, and a wrong chart is worse than a missing one. When a
measurement changes, change it in both places -- ``check_against_docs`` below asserts the headline
figures still appear in the document, so a drift is a test failure rather than a surprise.

Design decisions worth not relitigating:

* **Light surface only.** These render on GitHub inside an ``<img>``, and GitHub's dark theme does
  not follow the OS colour-scheme preference, so a ``prefers-color-scheme`` block inside the SVG
  would be wrong for exactly the readers it claims to serve. Same call as ``docs/architecture.svg``.
* **No hover layer**, for the same reason -- an ``<img>`` runs no script. Every value is printed on
  its mark, and the tables in ``docs/benchmarks.md`` are the table view.
* Two series at most, blue then orange, in that fixed order. Validated for colour-vision
  deficiency separation (worst pair ΔE 24.7 protan) rather than eyeballed.

Usage::

    tools/.venv/bin/python tools/bench/plot.py            # writes docs/charts/*.svg
    tools/.venv/bin/python tools/bench/plot.py --check    # verifies the figures against the docs
"""

from __future__ import annotations

import argparse
import html
import sys
from dataclasses import dataclass, field
from pathlib import Path
from xml.etree import ElementTree

REPO = Path(__file__).resolve().parents[2]
OUT_DIR = REPO / "docs" / "charts"
BENCHMARKS = REPO / "docs" / "benchmarks.md"

# ---------------------------------------------------------------------------
# Palette and chrome. Text never wears a series colour; marks carry identity.
# ---------------------------------------------------------------------------

SURFACE = "#fcfcfb"
INK = "#0b0b0b"
INK_SECONDARY = "#52514e"
INK_MUTED = "#898781"
GRID = "#e1e0d9"
BASELINE = "#c3c2b7"
TRACK = "#f0efec"  # the "not measured yet" slot
SERIES = ("#2a78d6", "#eb6834")  # blue, orange -- fixed order, never cycled

# Single quotes inside the stack on purpose: this string is interpolated into a double-quoted XML
# attribute, and the double-quoted form of "Segoe UI" closes the attribute and produces an SVG that
# no renderer will parse.
FONT = "system-ui, -apple-system, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif"

BAR_H = 22  # <= 24px by spec; the leftover band is deliberate air
RADIUS = 4


@dataclass
class Row:
    """One bar. ``value=None`` renders an empty track — a measurement not taken."""

    label: str
    value: float | None
    display: str = ""
    note: str = ""


@dataclass
class Chart:
    """One chart: its data, its axis, and the prose that frames it."""

    slug: str
    title: str
    subtitle: str
    rows: list[Row]
    unit: str
    ticks: list[float]
    reference: tuple[float, str] | None = None
    footnote: str = ""
    width: int = 780
    label_w: int = 232
    series: list[str] = field(default_factory=list)


def esc(text: str) -> str:
    """Escape for both text nodes and attribute values, quotes included."""
    return html.escape(text, quote=True)


def text_el(
    x: float,
    y: float,
    content: str,
    *,
    size: int,
    fill: str,
    weight: int = 400,
    anchor: str = "start",
) -> str:
    """One SVG text node, with the font stack and escaping applied in one place."""
    return (
        f'<text x="{x:.1f}" y="{y:.1f}" font-family="{FONT}" font-size="{size}" '
        f'font-weight="{weight}" fill="{fill}" text-anchor="{anchor}">{esc(content)}</text>'
    )


def bar_path(x0: float, y: float, length: float, height: float) -> str:
    """A bar squared at the baseline and rounded at the data end.

    Below the corner radius there is no room to round, so the bar degrades to a plain rectangle
    rather than to a malformed path — which is what a 0.07 ms bar next to a 150 ms axis needs.
    """
    if length <= RADIUS:
        return f'<rect x="{x0:.1f}" y="{y:.1f}" width="{max(length, 1.5):.1f}" height="{height}" />'
    x1 = x0 + length
    r = RADIUS
    return (
        f'<path d="M {x0:.1f} {y:.1f} H {x1 - r:.1f} A {r} {r} 0 0 1 {x1:.1f} {y + r:.1f} '
        f"V {y + height - r:.1f} A {r} {r} 0 0 1 {x1 - r:.1f} {y + height:.1f} "
        f'H {x0:.1f} Z" />'
    )


def render_bars(chart: Chart) -> str:
    """Horizontal bars: one series, category labels on the left, value at each tip."""
    row_h = 44
    top = 96 if chart.subtitle else 74
    plot_x = chart.label_w
    plot_w = chart.width - plot_x - 96
    plot_h = row_h * len(chart.rows)
    axis_y = top + plot_h
    height = axis_y + 46 + (26 if chart.footnote else 0) + (26 if chart.reference else 0)
    scale = plot_w / chart.ticks[-1]

    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {chart.width} {height}" '
        f'width="{chart.width}" height="{height}" role="img" aria-label="{esc(chart.title)}">',
        f'<rect width="{chart.width}" height="{height}" fill="{SURFACE}"/>',
        text_el(32, 40, chart.title, size=17, fill=INK, weight=650),
    ]
    if chart.subtitle:
        out.append(text_el(32, 64, chart.subtitle, size=13, fill=INK_SECONDARY))

    # Gridlines first, so the data sits on top of the chrome rather than under it.
    for tick in chart.ticks:
        x = plot_x + tick * scale
        out.append(
            f'<line x1="{x:.1f}" y1="{top - 8:.1f}" x2="{x:.1f}" y2="{axis_y:.1f}" '
            f'stroke="{GRID}" stroke-width="1"/>'
        )
        out.append(text_el(x, axis_y + 20, fmt(tick), size=12, fill=INK_MUTED, anchor="middle"))
    out.append(
        f'<line x1="{plot_x:.1f}" y1="{top - 8:.1f}" x2="{plot_x:.1f}" y2="{axis_y:.1f}" '
        f'stroke="{BASELINE}" stroke-width="1"/>'
    )
    out.append(
        text_el(chart.width - 32, axis_y + 20, chart.unit, size=12, fill=INK_MUTED, anchor="end")
    )

    for i, row in enumerate(chart.rows):
        y = top + i * row_h + (row_h - BAR_H) / 2
        out.append(
            text_el(plot_x - 16, y + 15, row.label, size=13, fill=INK_SECONDARY, anchor="end")
        )
        if row.value is None:
            out.append(
                f'<rect x="{plot_x:.1f}" y="{y:.1f}" width="{plot_w:.1f}" height="{BAR_H}" '
                f'rx="{RADIUS}" fill="{TRACK}"/>'
            )
            out.append(text_el(plot_x + 12, y + 15, row.note, size=12, fill=INK_MUTED))
            continue
        length = row.value * scale
        out.append(f'<g fill="{SERIES[0]}">{bar_path(plot_x, y, length, BAR_H)}</g>')
        out.append(
            text_el(
                plot_x + length + 10,
                y + 15,
                row.display or fmt(row.value),
                size=13,
                fill=INK,
                weight=650,
            )
        )
        if row.note:
            # A label is never clipped by the edge it ran into. Outside the bar while it fits;
            # once the bar is long enough to push it off the canvas, inside the bar end instead,
            # in white — which clears contrast on every series colour used here.
            note_x = plot_x + length + 18 + 7.2 * len(row.display or fmt(row.value))
            if note_x + 7.0 * len(row.note) < chart.width - 24:
                out.append(text_el(note_x, y + 15, row.note, size=12, fill=INK_MUTED))
            else:
                out.append(
                    text_el(
                        plot_x + length - 12,
                        y + 15,
                        row.note,
                        size=12,
                        fill="#ffffff",
                        anchor="end",
                    )
                )

    if chart.reference:
        # The threshold label lives below the axis, not above the plot: above, it lands on the
        # subtitle whenever the threshold sits at the left of the scale, and two lines of prose
        # overlapping is worse than a slightly longer chart.
        value, label = chart.reference
        x = plot_x + value * scale
        out.append(
            f'<line x1="{x:.1f}" y1="{top - 8:.1f}" x2="{x:.1f}" y2="{axis_y:.1f}" '
            f'stroke="{INK_MUTED}" stroke-width="1.5"/>'
        )
        anchor = "start" if x < plot_x + plot_w / 2 else "end"
        offset = 6 if anchor == "start" else -6
        out.append(text_el(x + offset, axis_y + 38, label, size=12, fill=INK_MUTED, anchor=anchor))

    if chart.footnote:
        out.append(text_el(32, height - 24, chart.footnote, size=12, fill=INK_MUTED))
    out.append("</svg>")
    return "\n".join(out)


def render_columns(chart: Chart) -> str:
    """Grouped columns for two series, with a legend — identity is never colour alone."""
    group_w = 150
    bar_w = 22
    gap = 2  # the surface gap that separates touching bars; never a stroke
    top = 118
    plot_h = 240
    axis_y = top + plot_h
    height = axis_y + (96 if chart.footnote else 74)
    width = 96 + group_w * len(chart.rows)
    scale = plot_h / chart.ticks[-1]

    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
        f'width="{width}" height="{height}" role="img" aria-label="{esc(chart.title)}">',
        f'<rect width="{width}" height="{height}" fill="{SURFACE}"/>',
        text_el(32, 40, chart.title, size=17, fill=INK, weight=650),
        text_el(32, 64, chart.subtitle, size=13, fill=INK_SECONDARY),
    ]

    legend_x = 32
    for name, colour in zip(chart.series, SERIES, strict=False):
        out.append(f'<rect x="{legend_x}" y="80" width="10" height="10" rx="2" fill="{colour}"/>')
        out.append(text_el(legend_x + 16, 89, name, size=12, fill=INK_SECONDARY))
        legend_x += 26 + 7 * len(name)

    for tick in chart.ticks:
        y = axis_y - tick * scale
        out.append(
            f'<line x1="72" y1="{y:.1f}" x2="{width - 24}" y2="{y:.1f}" '
            f'stroke="{GRID}" stroke-width="1"/>'
        )
        out.append(text_el(60, y + 4, fmt(tick), size=12, fill=INK_MUTED, anchor="end"))
    out.append(
        f'<line x1="72" y1="{axis_y:.1f}" x2="{width - 24}" y2="{axis_y:.1f}" '
        f'stroke="{BASELINE}" stroke-width="1"/>'
    )

    for i, row in enumerate(chart.rows):
        centre = 72 + group_w * i + group_w / 2
        values = row.value  # a list, for grouped columns
        assert isinstance(values, list)
        span = len(values) * bar_w + (len(values) - 1) * gap
        x = centre - span / 2
        for value, colour in zip(values, SERIES, strict=False):
            length = value * scale
            y = axis_y - length
            out.append(
                f'<g fill="{colour}">'
                f'<path d="M {x:.1f} {axis_y:.1f} V {y + RADIUS:.1f} '
                f"A {RADIUS} {RADIUS} 0 0 1 {x + RADIUS:.1f} {y:.1f} "
                f"H {x + bar_w - RADIUS:.1f} A {RADIUS} {RADIUS} 0 0 1 "
                f"{x + bar_w:.1f} {y + RADIUS:.1f} "
                f'V {axis_y:.1f} Z"/></g>'
            )
            out.append(
                text_el(
                    x + bar_w / 2,
                    y - 8,
                    f"{value:.2f}",
                    size=12,
                    fill=INK,
                    weight=650,
                    anchor="middle",
                )
            )
            x += bar_w + gap
        for j, line in enumerate(row.label.split("|")):
            out.append(
                text_el(
                    centre,
                    axis_y + 22 + j * 16,
                    line,
                    size=12,
                    fill=INK_SECONDARY if j == 0 else INK_MUTED,
                    anchor="middle",
                )
            )

    if chart.footnote:
        out.append(text_el(32, height - 24, chart.footnote, size=12, fill=INK_MUTED))
    out.append("</svg>")
    return "\n".join(out)


def fmt(value: float) -> str:
    """Format an axis tick: integers bare, everything else at its shortest faithful form."""
    if value == int(value):
        return str(int(value))
    return f"{value:g}"


# ---------------------------------------------------------------------------
# The measurements. Mirror of docs/benchmarks.md — keep the two in step.
# ---------------------------------------------------------------------------

CHARTS: list[Chart] = [
    Chart(
        slug="latency-budget",
        title="What inspection costs, against the budget it was given",
        subtitle=(
            "Added p95 latency versus talking to the upstream directly. Dev machine, ~1 KB payload."
        ),
        rows=[
            Row("Proxy only, no inspection", 0.07, "+0.07 ms"),
            Row("Layer 1 + layer 2 (NER), CPU", 10.8, "+10.8 ms"),
        ],
        unit="ms",
        ticks=[0, 25, 50, 75, 100, 125, 150],
        reference=(150.0, "PoC budget 150 ms"),
        footnote=(
            "Layer 2 costs one inference and almost nothing else: "
            "--doctor reports 11.8 ms steady for the same model."
        ),
    ),
    Chart(
        slug="streaming-ttft",
        title="Time to first token, by streaming inspection strategy",
        subtitle=(
            "Total generation time is identical in all four cases (~511 ms). "
            "Only the wait for the first character moves."
        ),
        rows=[
            Row("Direct to upstream (baseline)", 0.3, "0.3 ms"),
            Row("passthrough — the default", 0.4, "0.4 ms"),
            Row("sliding_window", 92.5, "92.5 ms", "one sentence, whatever the length"),
            Row("buffer", 511.0, "511 ms", "the whole generation"),
        ],
        unit="ms, p50",
        ticks=[0, 100, 200, 300, 400, 500],
        footnote=(
            "buffer's penalty scales with the answer: a 2 000-token reply would mean "
            "roughly 24 seconds of blank screen."
        ),
        label_w=248,
    ),
    Chart(
        slug="device-latency",
        title="Steady-state inference, per device",
        subtitle="HerBERT INT8, sequence 128, measured by sentin-gateway --doctor.",
        rows=[
            Row("CPU — AMD Ryzen AI 7 350", 11.8, "11.8 ms"),
            Row("GPU — NVIDIA dGPU via OpenCL", 115.8, "115.8 ms"),
            Row("NPU — Intel", None, note="not measured — needs Intel hardware"),
        ],
        unit="ms",
        ticks=[0, 30, 60, 90, 120],
        footnote=(
            "The empty row is why the project needs Intel hardware: "
            "the dev machine's AMD NPU is invisible to OpenVINO. Filled in by device-latency-intel."
        ),
        label_w=248,
    ),
    Chart(
        slug="device-latency-intel",
        title="Steady-state inference on hardware that has all three devices",
        subtitle=(
            "HerBERT INT8, sequence 128, Intel Core Ultra 7 258V. One machine, so the rows compare."
        ),
        rows=[
            Row("NPU — Intel AI Boost", 5.9, "5.9 ms"),
            Row("GPU — Intel Arc 140V (iGPU)", 2.7, "2.7 ms"),
            Row("CPU — Core Ultra 7 258V", 23.6, "23.6 ms"),
        ],
        unit="ms",
        ticks=[0, 5, 10, 15, 20, 25],
        footnote=(
            "Latency is not what separates these devices — every one of them clears the "
            "budget. Energy is: see device-energy."
        ),
        label_w=248,
    ),
    Chart(
        slug="device-energy",
        title="Energy per inference at 10 requests per second",
        subtitle=(
            "The load a gateway in front of a few agents sees. Median of 5 repeats, "
            "package RAPL, idle subtracted."
        ),
        rows=[
            Row("NPU — Intel AI Boost", 78.21, "78.21 mJ", "0.78 W above idle"),
            Row("GPU — Intel Arc 140V (iGPU)", 160.08, "160.08 mJ", "1.53 W above idle"),
            Row("CPU — Core Ultra 7 258V", 724.14, "724.14 mJ", "6.92 W above idle"),
        ],
        unit="mJ",
        ticks=[0, 200, 400, 600, 800],
        footnote=(
            "At saturation these two are 5% apart. At a realistic rate the iGPU still "
            "pays to be clocked up."
        ),
        label_w=248,
    ),
    Chart(
        slug="device-energy-saturation",
        title="Energy per inference at saturation, and why repeats were needed",
        subtitle=(
            "Driven as fast as each device will go. Median of 5 repeats after a discarded warm-up."
        ),
        rows=[
            Row("NPU — Intel AI Boost", 46.81, "46.81 mJ", "range 45.74-47.66"),
            Row("GPU — Intel Arc 140V (iGPU)", 49.51, "49.51 mJ", "range 49.15-49.99"),
            Row("CPU — Core Ultra 7 258V", 554.60, "554.60 mJ", "range 548.44-557.53"),
        ],
        unit="mJ",
        ticks=[0, 150, 300, 450, 600],
        footnote=(
            "One pass put these 3.7% apart, inside the platform's drift. Five repeats "
            "separate them cleanly."
        ),
        label_w=248,
    ),
    Chart(
        slug="layer1-throughput",
        title="Deterministic layer throughput, by what the text contains",
        subtitle=(
            "100 KB inputs, criterion, 100 samples per case. Throughput is flat in input size."
        ),
        rows=[
            Row("Prose, no identifiers", 1249, "1 249 MiB/s"),
            Row("Prose with identifiers", 934, "934 MiB/s"),
            Row("Digit noise (worst case)", 308, "308 MiB/s"),
        ],
        unit="MiB/s",
        ticks=[0, 250, 500, 750, 1000, 1250],
        reference=(100.0, "threshold 100 MB/s"),
        footnote=(
            "Every token in the worst case is a checksum candidate. "
            "Real traffic looks like the first two rows."
        ),
        label_w=232,
    ),
]

QUALITY = Chart(
    slug="model-quality",
    title="NER quality: why HerBERT, and what INT8 costs",
    subtitle="WikiANN, 500 sentences per language, exact span match on PER/ORG/LOC. F1.",
    rows=[
        Row("HerBERT|FP32, 476 MB", [88.06, 58.97]),
        Row("HerBERT|INT8, 123 MB", [87.57, 59.51]),
        Row("XLM-R|INT8, 284 MB", [62.62, 53.36]),
    ],
    unit="F1",
    ticks=[0, 25, 50, 75, 100],
    series=["Polish", "English"],
    footnote=(
        "Quantization is free at this scale; English is the model's real weakness, "
        "and a stated limitation."
    ),
)


def check_against_docs() -> int:
    """Fail if a headline figure no longer appears in docs/benchmarks.md.

    A chart that has quietly drifted from the document it illustrates is worse than no chart, and
    the drift is invisible in review — the SVG diff looks like a number changing, which is exactly
    what an intentional update looks like too.
    """
    doc = BENCHMARKS.read_text(encoding="utf-8")
    expected = [
        "+0.07 ms",
        "10.872",
        "92.5",
        "511.0",
        "11.8 ms",
        "115.8",
        "88.06",
        "87.57",
        "62.62",
        "308",
        # Phase 5, Intel Core Ultra 7 258V
        "554.60",
        "49.51",
        "46.81",
        "78.21",
        "160.08",
        "724.14",
        "+3.85 ms",
        "23.6 ms",
    ]
    missing = [needle for needle in expected if needle not in doc]
    if missing:
        print(f"figures not found in {BENCHMARKS.name}: {missing}", file=sys.stderr)
        return 1
    print(f"all {len(expected)} headline figures still present in {BENCHMARKS.name}")
    return 0


def main() -> int:
    """Render every chart, or with ``--check`` only verify the figures against the docs."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true", help="verify the figures against docs/benchmarks.md"
    )
    args = parser.parse_args()
    if args.check:
        return check_against_docs()

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    written = [(chart.slug, render_bars(chart)) for chart in CHARTS]
    written.append((QUALITY.slug, render_columns(QUALITY)))
    for slug, svg in written:
        # Parse before writing. A malformed SVG does not fail loudly anywhere downstream -- GitHub
        # simply shows a broken image, which is easy to miss in a long document. This cost one
        # round trip already: an unescaped double quote inside the font stack closed the attribute.
        ElementTree.fromstring(svg)
        # Trailing newline: the end-of-file pre-commit hook adds one otherwise, and generated
        # output that a hook has to fix means every regeneration shows up as a spurious diff.
        (OUT_DIR / f"{slug}.svg").write_text(svg + "\n", encoding="utf-8")
        print(f"wrote docs/charts/{slug}.svg")
    return check_against_docs()


if __name__ == "__main__":
    raise SystemExit(main())
