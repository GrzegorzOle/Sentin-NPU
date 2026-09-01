#!/usr/bin/env bash
# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
#
# Pack the OpenVINO IR models as standalone release assets.
#
# The IR is gitignored — hundreds of megabytes produced by the Python toolchain — so the only way
# to get it without running that toolchain has been to unpack a diagnostic bundle and dig the
# model out of it. NOTICE.md has claimed since Phase 1 that "the converted artifacts are
# distributed as GitHub Release assets"; this script is what makes that true.
#
# The bundles keep their own copy. They must stay self-contained: the target is a client
# workstation with nothing installed and possibly no network, and asking someone to fetch a second
# archive before the first one works would defeat that. These assets are for the other audience —
# somebody who wants the IR alone, to load from Python, to compare against their own conversion,
# or to re-run the quality measurements without a 1.4 GB FP32 detour.
#
# Names are deliberately NOT versioned. The model is an input to the project, not a product of it:
# the same HF revision through the same toolchain gives the same IR whatever the gateway's version
# happens to be. A stable name keeps
#   https://github.com/GrzegorzOle/Sentin-NPU/releases/latest/download/<name>.tar.gz
# valid forever, which a versioned name does not — the README's bundle URL had already gone stale
# by one release. Provenance and the release it shipped with go inside the archive instead, and
# SHA256SUMS.txt pins the bytes per release.

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${REPO}/dist"
VERSION="${1:-0.2.1}"

# INT8 only, both shape variants. FP32 stays out: at 477 MB per variant it is four times the size
# for something nobody runs — it is an intermediate that `prepare_model.py` regenerates, and the
# INT8 models are what the gateway loads and what every published number was measured on.
VARIANTS=(seq128 seq512)

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

# Recorded into each archive so a model that has been separated from its release can still say
# where it came from. Empty if the toolchain is not installed — the assets are often built in CI
# right after the IR is produced, but not always.
ov_version() {
    local py="${REPO}/tools/.venv/bin/python"
    if [ -x "${py}" ]; then
        "${py}" -c 'import openvino; print(openvino.__version__)' 2>/dev/null || echo "unknown"
    else
        echo "unknown"
    fi
}

write_model_readme() {
    local dir="$1" seq="$2" ov="$3"
    cat > "${dir}/README.txt" <<EOF
Sentin-NPU — HerBERT NER, OpenVINO IR, INT8, static shape [1, ${seq}]
=====================================================================

Shipped with release ${VERSION}. Converted and quantized by this project's Python toolchain
with OpenVINO ${ov}.

Source model
------------
  pczarnik/herbert-base-ner
  https://huggingface.co/pczarnik/herbert-base-ner
  Creative Commons Attribution 4.0 International (CC-BY-4.0) — commercial use permitted
  with attribution. See LICENSE-MODEL.txt.

  Derived from HerBERT (Allegro), a Polish language model. Entity types: PER, ORG, LOC.

Contents
--------
  openvino_model.xml / .bin   the IR itself
  config.json                 label map and model configuration
  tokenizer.json              fast tokenizer — REQUIRED at runtime, not optional

  The gateway loads the IR and the tokenizer from one directory. A directory holding only the
  .xml and .bin compiles fine in a diagnostic and then fails to serve, which is a confusing way
  to find out something is missing.

Two things that will bite you if you load this yourself
-------------------------------------------------------
1. The IR declares token_type_ids, but the HerBERT tokenizer does not emit them. Supply them
   explicitly as zeros. The FP32 graph tolerates the omission silently; this INT8 graph fails
   with an opaque eltwise shape error.

2. The shape is static, [1, ${seq}], on purpose — an NPU requirement. Pad and truncate to exactly
   ${seq} tokens. Quantization resets inputs to dynamic and the toolchain restores the static
   shape afterwards, so do not "fix" a dynamic model by reshaping a quantized one yourself
   without checking what came out.

Use it
------
  # with the gateway: point config.yaml at the directory you extracted
  inference:
    device: AUTO                          # NPU > GPU > CPU
    model_dir: /path/to/this/directory    # absolute, or it resolves against the working directory

  # from Python
  import openvino as ov
  m = ov.Core().compile_model("openvino_model.xml", "CPU")   # or NPU / GPU / AUTO

Quality
-------
WikiANN, 500 sentences per language, exact span match: F1 87.57 PL / 59.51 EN.
English is weak and that is measured, not a packaging accident — the model is Polish-first.
Quantization cost: ΔF1 -0.49 pp PL, +0.54 pp EN against FP32.

Which variant
-------------
seq128 is the default and the one every published latency figure uses. seq512 is for longer
inputs; it is the same model at a larger static shape, so it costs proportionally more per
inference whether or not the text fills it.
EOF
}

pack_variant() {
    local seq="$1" ov="$2"
    local src="${REPO}/models/herbert/int8/${seq}"
    local name="sentin-npu-model-herbert-int8-${seq}"
    local stage="${OUT}/${name}"

    if [ ! -f "${src}/openvino_model.xml" ]; then
        echo "WARNING: no IR at ${src} — skipping ${name}" >&2
        return 0
    fi

    say "model: staging ${seq}"
    rm -rf "${stage}"; mkdir -p "${stage}"

    # Same optional-file handling as the bundles: `if` rather than `[ -f ] && cp`, because under
    # `set -e` the short-circuit form aborts the script the first time an optional file is absent.
    # seq512 has no special_tokens_map.json or vocab.json, so this is the normal case, not an edge.
    local packed=0
    for f in openvino_model.xml openvino_model.bin config.json tokenizer.json \
             tokenizer_config.json special_tokens_map.json vocab.json merges.txt; do
        if [ -f "${src}/${f}" ]; then
            cp "${src}/${f}" "${stage}/"
            packed=$((packed + 1))
        fi
    done

    # The tokenizer is the one file whose absence survives every check that only compiles the
    # graph, so refuse rather than ship a model that cannot serve a request.
    [ -f "${stage}/tokenizer.json" ] || {
        echo "ERROR: ${src} has no tokenizer.json — the model would not be loadable" >&2
        exit 1
    }

    # CC-BY-4.0 requires attribution to travel with the work, and a release asset gets separated
    # from the repository the moment somebody downloads it.
    cp "${REPO}/NOTICE.md" "${stage}/LICENSE-MODEL.txt"
    write_model_readme "${stage}" "${seq#seq}" "${ov}"

    say "model: packing ${seq} (${packed} files)"
    tar -czf "${OUT}/${name}.tar.gz" -C "${OUT}" "${name}"
    rm -rf "${stage}"
}

mkdir -p "${OUT}"
OV="$(ov_version)"
for v in "${VARIANTS[@]}"; do
    pack_variant "${v}" "${OV}"
done

say "model assets:"
ls -lh "${OUT}"/sentin-npu-model-*.tar.gz 2>/dev/null | awk '{printf "  %-10s %s\n", $5, $NF}'
