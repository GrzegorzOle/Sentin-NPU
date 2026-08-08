# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
"""Quantize an exported IR variant to INT8.

    tools/.venv/bin/python tools/quantize.py --model herbert --seq 128

Full post-training quantization (weights *and* activations) needs calibration data, so this pulls
a few hundred sentences from WikiANN at run time. That data is never written into
``tests/fixtures/``: the repo's own fixtures stay fully synthetic, while calibration wants real
text whose token distribution actually resembles production traffic. Calibrating on templated
synthetic sentences would bias the activation ranges and quietly degrade the quantized model.

If full PTQ proves unstable, ``--weights-only`` falls back to weight compression, which needs no
calibration. That is a materially different result -- it changes what to expect from the NPU --
so the fallback is recorded in docs/benchmarks.md rather than silently substituted.
"""

from __future__ import annotations

import argparse
import logging
import shutil
from pathlib import Path

import openvino as ov
from datasets import Dataset, concatenate_datasets
from optimum.intel import (
    OVConfig,
    OVModelForTokenClassification,
    OVQuantizationConfig,
    OVQuantizer,
    OVWeightQuantizationConfig,
)
from transformers import AutoTokenizer, PreTrainedTokenizerBase

import model_registry as reg

logger = logging.getLogger("quantize")

#: WikiANN is Wikipedia-derived and covers both target languages. Used for calibration only --
#: never committed, never treated as a fixture.
CALIBRATION_DATASET = "unimelb-nlp/wikiann"
CALIBRATION_LANGUAGES = ("pl", "en")


def required_inputs(model_path: Path) -> set[str]:
    """Input tensor names the compiled IR actually expects.

    Candidates differ: the BERT-based HerBERT needs ``token_type_ids``, XLM-R does not. Feeding
    the wrong set fails deep inside the graph with an opaque shape-inference error in the
    embeddings, so the calibration data is built from what the IR declares rather than assumed.
    """
    ir = ov.Core().read_model(model_path / "openvino_model.xml")
    return {inp.get_any_name() for inp in ir.inputs}


def build_calibration_dataset(
    quantizer: OVQuantizer,
    tokenizer: PreTrainedTokenizerBase,
    seq: int,
    num_samples: int,
    wanted: set[str],
) -> Dataset:
    """Draw an equal number of PL and EN sentences and tokenize them to the static shape."""

    def preprocess(examples: dict) -> dict:
        # WikiANN stores pre-tokenized words; rejoin them so the model's own tokenizer decides
        # the subword split, exactly as it will at inference time.
        texts = [" ".join(tokens) for tokens in examples["tokens"]]
        return tokenizer(
            texts,
            padding="max_length",
            truncation=True,
            max_length=seq,
            return_token_type_ids="token_type_ids" in wanted,
        )

    per_language = max(1, num_samples // len(CALIBRATION_LANGUAGES))
    parts = []
    for lang in CALIBRATION_LANGUAGES:
        logger.info("calibration: %s %s x%d", CALIBRATION_DATASET, lang, per_language)
        parts.append(
            quantizer.get_calibration_dataset(
                CALIBRATION_DATASET,
                dataset_config_name=lang,
                dataset_split="train",
                num_samples=per_language,
                preprocess_function=preprocess,
                preprocess_batch=True,
                trust_remote_code=False,
            )
        )
    combined = concatenate_datasets(parts)
    # Only the tensors the model consumes may remain, or NNCF chokes on stray columns.
    return combined.remove_columns([c for c in combined.column_names if c not in wanted])


def quantize(key: str, seq: int, num_samples: int, *, weights_only: bool, overwrite: bool) -> None:
    source = reg.model_dir(key, "fp32", seq)
    target = reg.model_dir(key, "int8", seq)
    if not (source / "openvino_model.xml").exists():
        raise SystemExit(f"{source} not found -- run prepare_model.py --model {key} first")
    if target.exists() and not overwrite:
        logger.info("%s already exists, skipping (use --overwrite)", target)
        return

    tokenizer = AutoTokenizer.from_pretrained(source)
    model = OVModelForTokenClassification.from_pretrained(source)
    quantizer = OVQuantizer.from_pretrained(model)

    if weights_only:
        logger.info("weight-only INT8 (no calibration)")
        config = OVConfig(quantization_config=OVWeightQuantizationConfig(bits=8))
        calibration = None
    else:
        config = OVConfig(quantization_config=OVQuantizationConfig(bits=8, num_samples=num_samples))
        calibration = build_calibration_dataset(
            quantizer, tokenizer, seq, num_samples, required_inputs(source)
        )

    target.mkdir(parents=True, exist_ok=True)
    quantizer.quantize(calibration_dataset=calibration, save_directory=target, ov_config=config)
    tokenizer.save_pretrained(target)
    # The scorer reads id2label from config.json; the quantizer does not always carry it over.
    if not (target / "config.json").exists():
        shutil.copy(source / "config.json", target / "config.json")

    fp32_mb, int8_mb = reg.dir_size_mb(source), reg.dir_size_mb(target)
    logger.info(
        "%s seq%d: %.1f MB -> %.1f MB (%.1f%% of FP32)",
        key,
        seq,
        fp32_mb,
        int8_mb,
        100 * int8_mb / fp32_mb,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="all", help="candidate key or 'all'")
    parser.add_argument("--seq", type=int, action="append", help="sequence length; repeatable")
    parser.add_argument("--num-samples", type=int, default=300, help="calibration sentences")
    parser.add_argument(
        "--weights-only",
        action="store_true",
        help="compress weights only; no calibration data needed",
    )
    parser.add_argument("--overwrite", action="store_true")
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")

    keys = sorted(reg.CANDIDATES) if args.model == "all" else [args.model]
    lengths = tuple(args.seq) if args.seq else reg.SEQUENCE_LENGTHS
    for key in keys:
        for seq in lengths:
            quantize(
                key,
                seq,
                args.num_samples,
                weights_only=args.weights_only,
                overwrite=args.overwrite,
            )


if __name__ == "__main__":
    main()
