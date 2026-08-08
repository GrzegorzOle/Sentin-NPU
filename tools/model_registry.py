# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
"""Single source of truth about the NER model candidates evaluated in research question B1.

Every toolchain script takes ``--model <id>`` and looks the details up here, so the HuggingFace
repository ids, licences and label sets are stated exactly once. The B1 comparison table in
``docs/benchmarks.md`` is generated from these entries plus measured numbers, rather than being
maintained by hand.

Licence filtering already happened and is recorded in ``notes``: candidates whose licence forbids
commercial use, or which carry no licence at all, are not listed here at all. Do not add a model
without checking its licence first -- the project requires commercial use to be permitted.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

#: Repo root, derived from this file's location so the scripts work from any working directory.
REPO_ROOT = Path(__file__).resolve().parent.parent

#: Entity types the gateway actually consumes. Candidates may predict more (XLM-R also emits
#: DATE); scoring is restricted to this intersection so the B1 comparison stays fair.
SCORED_ENTITIES: tuple[str, ...] = ("PER", "ORG", "LOC")

#: Static sequence lengths to export. Static shapes are an NPU requirement (Phase 5); the shorter
#: variant is preferred at run time whenever the text fits.
SEQUENCE_LENGTHS: tuple[int, ...] = (128, 512)


@dataclass(frozen=True)
class Candidate:
    """One B1 candidate model."""

    key: str
    repo: str
    licence: str
    #: Languages the NER head was actually fine-tuned on -- not what the base model supports.
    finetuned_languages: tuple[str, ...]
    labels: tuple[str, ...]
    #: True when the repo ships tokenizer.json, i.e. the Rust `tokenizers` crate can load it
    #: directly in Phase 4. Otherwise the sentencepiece model needs converting first.
    has_fast_tokenizer_json: bool
    notes: str = ""
    extra: dict[str, str] = field(default_factory=dict)

    @property
    def scored_labels(self) -> tuple[str, ...]:
        """Entity types this model can be scored on, in the shared B1 label space."""
        present = {label.split("-", 1)[1] for label in self.labels if "-" in label}
        return tuple(e for e in SCORED_ENTITIES if e in present)


CANDIDATES: dict[str, Candidate] = {
    "herbert": Candidate(
        key="herbert",
        repo="pczarnik/herbert-base-ner",
        licence="CC-BY-4.0",
        finetuned_languages=("pl",),
        labels=("O", "B-PER", "I-PER", "B-ORG", "I-ORG", "B-LOC", "I-LOC"),
        has_fast_tokenizer_json=True,
        notes=(
            "Polish-only. Label set is exactly what the gateway needs. Ships tokenizer.json, "
            "so Phase 4 can load it in Rust without converting sentencepiece."
        ),
    ),
    "xlmr": Candidate(
        key="xlmr",
        repo="Davlan/xlm-roberta-base-ner-hrl",
        licence="AFL-3.0",
        # The 'hrl' fine-tune set does NOT include Polish -- Polish performance, if any, comes
        # from XLM-R's multilingual pretraining transferring zero-shot. That is precisely what
        # B1 has to measure rather than assume.
        finetuned_languages=("ar", "de", "en", "es", "fr", "it", "lv", "nl", "pt", "zh"),
        labels=(
            "O",
            "B-DATE",
            "I-DATE",
            "B-PER",
            "I-PER",
            "B-ORG",
            "I-ORG",
            "B-LOC",
            "I-LOC",
        ),
        has_fast_tokenizer_json=False,
        notes=(
            "Multilingual, but Polish was not in the fine-tune set. Also predicts DATE, which is "
            "excluded from scoring. No tokenizer.json -- Phase 4 would have to convert "
            "sentencepiece.bpe.model."
        ),
    ),
}


def get(key: str) -> Candidate:
    """Look up a candidate by its short key, with a helpful error when it is unknown."""
    try:
        return CANDIDATES[key]
    except KeyError:
        known = ", ".join(sorted(CANDIDATES))
        raise SystemExit(f"unknown model '{key}'; known candidates: {known}") from None


def model_dir(key: str, precision: str, sequence_length: int) -> Path:
    """Absolute path an IR variant is written to.

    ``models/`` is gitignored on purpose: IR ships as GitHub Release assets, never in the repo.
    """
    return REPO_ROOT / "models" / key / precision / f"seq{sequence_length}"


def dir_size_mb(path: Path) -> float:
    """Total size of a model directory in MB, for the size columns of the B1 table."""
    return sum(f.stat().st_size for f in path.rglob("*") if f.is_file()) / (1024 * 1024)
