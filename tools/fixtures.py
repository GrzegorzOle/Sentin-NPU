# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
"""Loader for the annotated NER fixtures in ``tests/fixtures/``.

Fixtures are stored in a **marked-up** form rather than as text plus offsets::

    {"lang": "pl", "marked": "Pan [PER:Marek Wiśniowiecki] mieszka w [LOC:Bydgoszczy]."}

The offsets are derived here. That is deliberate: hand-maintained character offsets drift the
moment anyone edits a sentence, and a fixture whose gold spans silently point at the wrong
characters produces confident, wrong F1 numbers. Markup cannot be inconsistent with its own text.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass

from model_registry import REPO_ROOT, SCORED_ENTITIES

FIXTURE_DIR = REPO_ROOT / "tests" / "fixtures"

#: ``[TYPE:surface form]`` -- no nesting, which the fixtures do not need.
_MARKER = re.compile(r"\[(?P<type>[A-Z]+):(?P<text>[^\]]+)\]")


@dataclass(frozen=True)
class Span:
    """One annotated entity: a half-open character range and its label.

    Character offsets, not bytes — the Python tokenizer bindings report characters, while the Rust
    ``tokenizers`` crate reports bytes. The two implementations index strings differently and each
    must convert on its own side.
    """

    start: int
    end: int
    label: str

    def key(self) -> tuple[int, int, str]:
        """Identity used for exact-match scoring."""
        return (self.start, self.end, self.label)


@dataclass(frozen=True)
class Example:
    """One evaluation sentence with its gold spans."""

    lang: str
    text: str
    spans: tuple[Span, ...]


def parse_marked(marked: str) -> tuple[str, tuple[Span, ...]]:
    """Strip ``[TYPE:...]`` markers, returning the plain text and the spans it annotated."""
    text_parts: list[str] = []
    spans: list[Span] = []
    cursor = 0
    plain_len = 0

    for match in _MARKER.finditer(marked):
        literal = marked[cursor : match.start()]
        text_parts.append(literal)
        plain_len += len(literal)

        surface = match.group("text")
        label = match.group("type")
        if label not in SCORED_ENTITIES:
            raise ValueError(f"unknown entity type {label!r} in fixture: {marked!r}")
        spans.append(Span(plain_len, plain_len + len(surface), label))

        text_parts.append(surface)
        plain_len += len(surface)
        cursor = match.end()

    text_parts.append(marked[cursor:])
    return "".join(text_parts), tuple(spans)


def load(lang: str) -> list[Example]:
    """Load one language's fixture file."""
    path = FIXTURE_DIR / f"ner_{lang}.jsonl"
    examples: list[Example] = []
    with path.open(encoding="utf-8") as handle:
        for line_no, line in enumerate(handle, start=1):
            line = line.strip()
            if not line or line.startswith("//"):
                continue
            try:
                record = json.loads(line)
                text, spans = parse_marked(record["marked"])
            except (ValueError, KeyError) as exc:
                raise ValueError(f"{path}:{line_no}: {exc}") from exc
            examples.append(Example(lang=lang, text=text, spans=spans))
    return examples


def load_all(languages: tuple[str, ...] = ("pl", "en")) -> dict[str, list[Example]]:
    """Load every fixture language, keyed by language code."""
    return {lang: load(lang) for lang in languages}


if __name__ == "__main__":
    # Sanity check: print what the parser actually extracted, so a bad marker is obvious.
    for lang, examples in load_all().items():
        total = sum(len(e.spans) for e in examples)
        print(f"{lang}: {len(examples)} sentences, {total} entities")
        for example in examples[:3]:
            rendered = [f"{example.text[s.start : s.end]}={s.label}" for s in example.spans]
            print(f"   {example.text}")
            print(f"      -> {rendered}")
