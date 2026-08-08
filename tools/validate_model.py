# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
"""Score an OpenVINO IR NER model against the fixtures, and compare FP32 with INT8 (metric M3).

    tools/.venv/bin/python tools/validate_model.py --model herbert --seq 128
    tools/.venv/bin/python tools/validate_model.py --all --json results.json

The span-mapping logic here is the reference implementation for Phase 4: the Rust bridge has to
reproduce exactly this behaviour -- offsets back to the *original* text, first-subword labelling,
and BIO merging -- or findings will point at the wrong characters. Keep the two in step.

Scoring is exact-span-match and restricted to ``model_registry.SCORED_ENTITIES``. Candidates
predict different label sets (XLM-R also emits DATE); scoring their intersection is what makes the
B1 comparison fair rather than a measure of who predicts more classes.
"""

from __future__ import annotations

import argparse
import json
import logging
from dataclasses import dataclass, replace
from pathlib import Path

import numpy as np
import openvino as ov
from transformers import AutoTokenizer

import model_registry as reg
from fixtures import Example, Span, load_all

logger = logging.getLogger("validate_model")


@dataclass(frozen=True)
class Score:
    """Exact-span-match counts for one evaluation, summable across languages and batches."""

    true_positives: int
    predicted: int
    gold: int

    @property
    def precision(self) -> float:
        """Share of predicted spans that were correct. Zero when nothing was predicted."""
        return self.true_positives / self.predicted if self.predicted else 0.0

    @property
    def recall(self) -> float:
        """Share of gold spans that were found. Zero when there were none."""
        return self.true_positives / self.gold if self.gold else 0.0

    @property
    def f1(self) -> float:
        """Harmonic mean of precision and recall — the figure quoted as model quality."""
        p, r = self.precision, self.recall
        return 2 * p * r / (p + r) if p + r else 0.0

    def __add__(self, other: Score) -> Score:
        return Score(
            self.true_positives + other.true_positives,
            self.predicted + other.predicted,
            self.gold + other.gold,
        )


class NerRunner:
    """Loads one IR variant and turns text into character spans."""

    def __init__(self, model_path: Path, sequence_length: int, device: str) -> None:
        self.sequence_length = sequence_length
        self.tokenizer = AutoTokenizer.from_pretrained(model_path)

        core = ov.Core()
        model = core.read_model(model_path / "openvino_model.xml")
        self.compiled = core.compile_model(model, device)
        # Which device actually ran the inference is a first-class fact for this project, not a
        # debug detail -- AUTO can silently fall back.
        self.device = self.compiled.get_property("EXECUTION_DEVICES")
        logger.info("%s: executing on %s", model_path, self.device)

        self.input_names = [inp.get_any_name() for inp in self.compiled.inputs]

        config = json.loads((model_path / "config.json").read_text(encoding="utf-8"))
        self.id2label = {int(k): v for k, v in config["id2label"].items()}
        self.truncated = 0

    def _logits(self, encoded: dict[str, np.ndarray]) -> np.ndarray:
        """Feed every input the IR declares, synthesising the ones the tokenizer omits.

        HerBERT's graph takes ``token_type_ids`` but its tokenizer does not emit them; XLM-R takes
        no such input at all. Quietly dropping a declared input is not an option -- the FP32 graph
        tolerated it (defaulting to zeros, which happens to be right for single-segment input)
        while the INT8 graph failed with an opaque eltwise shape error. Zeros are the correct
        value for a single sequence, so they are supplied explicitly rather than left to chance.
        """
        inputs: dict[str, np.ndarray] = {}
        for name in self.input_names:
            if name in encoded:
                inputs[name] = encoded[name]
            else:
                inputs[name] = np.zeros((1, self.sequence_length), dtype=np.int64)
        return next(iter(self.compiled(inputs).values()))

    def predict(self, text: str) -> list[Span]:
        """Run the model over one text and return character spans in the *original* string.

        This is the reference implementation the Rust bridge has to reproduce: a label comes from
        a word's first subword, but its extent must cover every subword, and offsets map back to
        the untokenized text. Getting the extent wrong truncates entities mid-word and cost 63 F1
        points before it was found.
        """
        encoding = self.tokenizer(
            text,
            return_offsets_mapping=True,
            return_tensors="np",
            padding="max_length",
            truncation=True,
            max_length=self.sequence_length,
        )
        if len(self.tokenizer(text)["input_ids"]) > self.sequence_length:
            self.truncated += 1

        offsets = encoding.pop("offset_mapping")[0]
        word_ids = encoding.word_ids(0)
        logits = self._logits({k: np.asarray(v) for k, v in encoding.items()})
        label_ids = logits[0].argmax(axis=-1)

        # Two different things are per-word here, and conflating them truncates spans:
        #   * the LABEL comes from the word's first subword (later subwords would vote
        #     independently and fragment the entity),
        #   * the CHARACTER EXTENT must span every subword, or an entity ends mid-word --
        #     "Marka Wiśniowieckiego" collapses to "Marka Wiśni" when only the first subword's
        #     offset is used. Phase 4's Rust implementation has to make the same distinction.
        extents: dict[int, list[int]] = {}
        labels: dict[int, str] = {}
        for position, word_id in enumerate(word_ids):
            if word_id is None:
                continue
            start, end = int(offsets[position][0]), int(offsets[position][1])
            if end <= start:  # special tokens carry an empty offset span
                continue
            if word_id in extents:
                extents[word_id][1] = max(extents[word_id][1], end)
            else:
                extents[word_id] = [start, end]
                labels[word_id] = self.id2label[int(label_ids[position])]

        words = [(extents[w][0], extents[w][1], labels[w]) for w in sorted(extents)]
        return _merge_bio(words)


def _merge_bio(words: list[tuple[int, int, str]]) -> list[Span]:
    """Merge word-level BIO tags into character spans, keeping only scored entity types."""
    spans: list[Span] = []
    current: Span | None = None

    for start, end, tag in words:
        prefix, _, entity = tag.partition("-")
        if not entity or entity not in reg.SCORED_ENTITIES:
            # Includes 'O' and any label outside the shared space (e.g. XLM-R's DATE).
            if current:
                spans.append(current)
                current = None
            continue

        if prefix == "B" or current is None or current.label != entity:
            if current:
                spans.append(current)
            current = Span(start, end, entity)
        else:  # I- continuing the same entity
            current = replace(current, end=end)

    if current:
        spans.append(current)
    return spans


def score(runner: NerRunner, examples: list[Example]) -> Score:
    """Score one model over one language, counting only ``SCORED_ENTITIES``.

    Candidates predict different label sets, so scoring their intersection is what makes the
    comparison a measure of quality rather than of how many classes each one emits.
    """
    total = Score(0, 0, 0)
    for example in examples:
        predicted = {s.key() for s in runner.predict(example.text)}
        gold = {s.key() for s in example.spans if s.label in reg.SCORED_ENTITIES}
        total = total + Score(len(predicted & gold), len(predicted), len(gold))
    return total


def load_examples(dataset: str, num_samples: int) -> dict[str, list[Example]]:
    """Fixtures (offline, synthetic, small) or WikiANN (downloaded, real, large enough for M3)."""
    if dataset == "fixtures":
        return load_all()
    import wikiann

    return wikiann.load_all(num_samples=num_samples)


def evaluate(
    key: str,
    precision: str,
    seq: int,
    device: str,
    examples_by_lang: dict[str, list[Example]],
) -> dict[str, object] | None:
    """Score one IR variant across every language, or return ``None`` if it is not built yet."""
    path = reg.model_dir(key, precision, seq)
    if not (path / "openvino_model.xml").exists():
        logger.warning("%s missing -- run prepare_model.py / quantize.py first", path)
        return None

    runner = NerRunner(path, seq, device)
    result: dict[str, object] = {
        "model": key,
        "precision": precision,
        "seq": seq,
        "device": runner.device,
        "size_mb": round(reg.dir_size_mb(path), 1),
    }
    for lang, examples in examples_by_lang.items():
        s = score(runner, examples)
        result[f"f1_{lang}"] = round(s.f1 * 100, 2)
        result[f"precision_{lang}"] = round(s.precision * 100, 2)
        result[f"recall_{lang}"] = round(s.recall * 100, 2)
        result[f"gold_{lang}"] = s.gold
    if runner.truncated:
        logger.warning("%s: %d sentences exceeded seq %d", key, runner.truncated, seq)
    return result


def main() -> None:
    """Score the requested variants and print the comparison table, including the M3 verdict."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="all", help="candidate key or 'all'")
    parser.add_argument("--seq", type=int, default=128)
    parser.add_argument("--precision", action="append", help="fp32 and/or int8; default both")
    parser.add_argument("--device", default="CPU", help="NPU | GPU | CPU | AUTO")
    parser.add_argument(
        "--dataset",
        default="fixtures",
        choices=("fixtures", "wikiann"),
        help="fixtures = offline synthetic; wikiann = downloaded, large enough to resolve M3",
    )
    parser.add_argument("--num-samples", type=int, default=500, help="wikiann sentences per lang")
    parser.add_argument("--json", type=Path, help="also write raw results here")
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")

    keys = sorted(reg.CANDIDATES) if args.model == "all" else [args.model]
    precisions = args.precision or ["fp32", "int8"]
    examples = load_examples(args.dataset, args.num_samples)
    counts = {lang: sum(len(e.spans) for e in ex) for lang, ex in examples.items()}
    logger.info("dataset=%s gold entities per language: %s", args.dataset, counts)

    results = [
        r
        for key in keys
        for precision in precisions
        if (r := evaluate(key, precision, args.seq, args.device, examples)) is not None
    ]

    if not results:
        raise SystemExit("no models evaluated -- nothing to report")

    header = f"{'model':<9}{'prec':<7}{'seq':<6}{'F1 pl':>8}{'F1 en':>8}{'size MB':>10}"
    print(f"\n{header}\n{'-' * len(header)}")
    for r in results:
        print(
            f"{r['model']:<9}{r['precision']:<7}{r['seq']:<6}"
            f"{r['f1_pl']:>8.2f}{r['f1_en']:>8.2f}{r['size_mb']:>10.1f}"
        )

    # M3: INT8 must not cost more than 2 percentage points of F1 versus FP32.
    print()
    for key in keys:
        fp32 = next((r for r in results if r["model"] == key and r["precision"] == "fp32"), None)
        int8 = next((r for r in results if r["model"] == key and r["precision"] == "int8"), None)
        if not (fp32 and int8):
            continue
        for lang in ("pl", "en"):
            delta = float(int8[f"f1_{lang}"]) - float(fp32[f"f1_{lang}"])
            verdict = "PASS" if abs(delta) < 2.0 else "FAIL"
            print(f"M3 {key} {lang}: dF1 = {delta:+.2f} pp  [{verdict}]")

    if args.json:
        args.json.write_text(json.dumps(results, indent=2), encoding="utf-8")
        logger.info("wrote %s", args.json)


if __name__ == "__main__":
    main()
