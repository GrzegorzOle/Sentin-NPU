# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
"""Convert a HuggingFace NER model to OpenVINO IR with static shapes.

One command per candidate produces every IR variant it needs::

    tools/.venv/bin/python tools/prepare_model.py --model herbert

**Static shapes are not an optimisation here, they are a requirement.** The OpenVINO NPU plugin
compiles for fixed input dimensions, so each model is exported once per sequence length in
``model_registry.SEQUENCE_LENGTHS``. The gateway picks the shortest variant a given text fits into.

Output goes to ``models/<key>/fp32/seq<N>/`` -- gitignored, because IR ships as GitHub Release
assets rather than in the repository.
"""

from __future__ import annotations

import argparse
import logging

from optimum.intel import OVModelForTokenClassification
from transformers import AutoTokenizer

import model_registry as reg

logger = logging.getLogger("prepare_model")


def export(key: str, sequence_lengths: tuple[int, ...], *, overwrite: bool) -> None:
    """Export one candidate to IR, once per requested sequence length."""
    candidate = reg.get(key)
    logger.info("exporting %s (%s, licence %s)", key, candidate.repo, candidate.licence)

    tokenizer = AutoTokenizer.from_pretrained(candidate.repo)

    # Convert from PyTorch exactly once. reshape() mutates the graph in place, so each variant is
    # then reloaded from this dynamic-shape IR -- cheap, and avoids reshaping an already-reshaped
    # graph or re-running the (slow) PyTorch export per sequence length.
    staging = reg.model_dir(key, "fp32", 0).parent / "_dynamic"
    if not staging.exists() or overwrite:
        staging.mkdir(parents=True, exist_ok=True)
        exported = OVModelForTokenClassification.from_pretrained(candidate.repo, export=True)
        exported.save_pretrained(staging)
        tokenizer.save_pretrained(staging)
        logger.info("converted to dynamic-shape IR at %s", staging)

    for seq in sequence_lengths:
        out = reg.model_dir(key, "fp32", seq)
        if out.exists() and not overwrite:
            logger.info("seq %d: %s already exists, skipping (use --overwrite)", seq, out)
            continue
        out.mkdir(parents=True, exist_ok=True)

        variant = OVModelForTokenClassification.from_pretrained(staging)
        variant.reshape(batch_size=1, sequence_length=seq)
        variant.save_pretrained(out)
        tokenizer.save_pretrained(out)

        logger.info("seq %d: wrote %s (%.1f MB)", seq, out, reg.dir_size_mb(out))


def main() -> None:
    """Convert the requested candidates from Hugging Face to OpenVINO IR at both shapes."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--model",
        default="all",
        help=f"candidate key ({', '.join(sorted(reg.CANDIDATES))}) or 'all'",
    )
    parser.add_argument(
        "--seq",
        type=int,
        action="append",
        help=f"sequence length; repeatable. Default: {list(reg.SEQUENCE_LENGTHS)}",
    )
    parser.add_argument("--overwrite", action="store_true", help="re-export existing variants")
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")

    keys = sorted(reg.CANDIDATES) if args.model == "all" else [args.model]
    lengths = tuple(args.seq) if args.seq else reg.SEQUENCE_LENGTHS
    for key in keys:
        export(key, lengths, overwrite=args.overwrite)


if __name__ == "__main__":
    main()
