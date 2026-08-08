# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
"""WikiANN as an external evaluation set, downloaded at run time.

The committed fixtures in ``tests/fixtures/`` are synthetic and deliberately small -- they exist
so the pipeline can be checked offline and without any real personal data in the repository. They
are *too* small to resolve a two-percentage-point threshold: with ~20 English entities, one
missed entity moves F1 by roughly five points, so an M3 verdict taken from them alone is noise.

WikiANN supplies the sample size needed for a defensible B1 comparison. It is downloaded to the
HuggingFace cache and never written into ``tests/``, so the repository's no-real-data rule is
untouched.
"""

from __future__ import annotations

from datasets import load_dataset

from fixtures import Example, Span

DATASET = "unimelb-nlp/wikiann"

#: WikiANN's fixed tag order.
TAGS = ("O", "B-PER", "I-PER", "B-ORG", "I-ORG", "B-LOC", "I-LOC")


def _to_example(tokens: list[str], tag_ids: list[int], lang: str) -> Example:
    """Rebuild plain text from pre-tokenized words, tracking each word's character span."""
    text_parts: list[str] = []
    extents: list[tuple[int, int]] = []
    cursor = 0
    for index, token in enumerate(tokens):
        if index:
            text_parts.append(" ")
            cursor += 1
        text_parts.append(token)
        extents.append((cursor, cursor + len(token)))
        cursor += len(token)

    spans: list[Span] = []
    current: Span | None = None
    for (start, end), tag_id in zip(extents, tag_ids, strict=True):
        prefix, _, entity = TAGS[tag_id].partition("-")
        if not entity:
            if current:
                spans.append(current)
                current = None
            continue
        if prefix == "B" or current is None or current.label != entity:
            if current:
                spans.append(current)
            current = Span(start, end, entity)
        else:
            current = Span(current.start, end, entity)
    if current:
        spans.append(current)

    return Example(lang=lang, text="".join(text_parts), spans=tuple(spans))


def load(lang: str, num_samples: int = 500, split: str = "test") -> list[Example]:
    """Load a slice of WikiANN for one language, in the same shape as the fixtures loader."""
    dataset = load_dataset(DATASET, lang, split=f"{split}[:{num_samples}]")
    return [_to_example(row["tokens"], row["ner_tags"], lang) for row in dataset]


def load_all(languages: tuple[str, ...] = ("pl", "en"), num_samples: int = 500) -> dict:
    """Load a slice of WikiANN per language — the set large enough for quantitative claims."""
    return {lang: load(lang, num_samples) for lang in languages}


if __name__ == "__main__":
    for lang, examples in load_all(num_samples=5).items():
        print(f"--- {lang}")
        for example in examples[:3]:
            rendered = [(example.text[s.start : s.end], s.label) for s in example.spans]
            print(f"   {example.text}\n      -> {rendered}")
