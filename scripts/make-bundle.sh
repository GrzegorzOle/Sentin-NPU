#!/usr/bin/env bash
# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
#
# Build a self-contained diagnostic bundle for a machine with no toolchain.
#
# The target is a plain client workstation: no Rust, no Python, no OpenVINO install, and
# possibly no network. Everything therefore travels in the archive, including the model.
#
# The binary is built inside a Debian bullseye container rather than on this host. A binary
# linked against Fedora's glibc 2.43 will not start on an older system, and "older" here
# includes every Ubuntu release; bullseye's 2.31 runs on anything newer, which is the direction
# glibc compatibility actually works.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${REPO_ROOT}/dist"
STAGE="${OUT_DIR}/sentin-npu-diag"
MODEL_ROOT="${REPO_ROOT}/models/herbert/int8"
OV_LIBS="${REPO_ROOT}/tools/.venv/lib/python3.11/site-packages/openvino/libs"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

say "building sentin-doctor against an old glibc (container)"
docker run --rm \
    -v "${REPO_ROOT}":/w:z -w /w/gateway \
    -e CARGO_TARGET_DIR=/w/gateway/target-bullseye \
    rust:1-bullseye \
    cargo build --release -p sentin-diag --bin sentin-doctor

BINARY="${REPO_ROOT}/gateway/target-bullseye/release/sentin-doctor"
[ -x "${BINARY}" ] || { echo "build produced no binary" >&2; exit 1; }
say "binary needs at most: $(objdump -T "${BINARY}" | grep -oE 'GLIBC_[0-9.]+' | sort -uV | tail -1)"

say "staging"
rm -rf "${STAGE}"
mkdir -p "${STAGE}/lib" "${STAGE}/models"
cp "${BINARY}" "${STAGE}/"

[ -d "${OV_LIBS}" ] || { echo "no OpenVINO libs at ${OV_LIBS}" >&2; exit 1; }
cp -a "${OV_LIBS}"/. "${STAGE}/lib/"

# The crate loads OpenVINO with dlopen and looks for *unversioned* sonames, which the Python
# wheel does not ship. Without these links the binary fails at startup claiming it cannot find a
# library that is plainly present.
say "creating unversioned symlinks"
( cd "${STAGE}/lib"
  for f in *.so.*; do
      [ -e "$f" ] || continue
      ln -sf "$f" "${f%%.so.*}.so"
  done )

for seq in 128 512; do
    src="${MODEL_ROOT}/seq${seq}"
    if [ -d "${src}" ]; then
        say "including model seq${seq}"
        mkdir -p "${STAGE}/models/seq${seq}"
        cp "${src}"/openvino_model.xml "${src}"/openvino_model.bin "${src}"/config.json \
           "${STAGE}/models/seq${seq}/"
    else
        echo "WARNING: no model at ${src} — run tools/quantize.py first" >&2
    fi
done

cp "${REPO_ROOT}/scripts/run-diagnostics.sh" "${STAGE}/run.sh"
chmod +x "${STAGE}/run.sh" "${STAGE}/sentin-doctor"

cat > "${STAGE}/README.txt" <<'EOF'
Sentin-NPU diagnostic bundle
============================

Self-contained: no Rust, no Python, no OpenVINO installation, no network needed.

    ./run.sh              device report only (about a minute)
    ./run.sh --power      also measure energy per device (adds a few minutes)

Everything is written to results/ and collected into one .tar.gz to send back.

If the NPU does not appear, the report says so and why. That outcome is a result,
not a failure — please send it either way.
EOF

say "packing"
ARCHIVE="${OUT_DIR}/sentin-npu-diag.tar.gz"
tar -czf "${ARCHIVE}" -C "${OUT_DIR}" sentin-npu-diag
say "done: ${ARCHIVE} ($(du -h "${ARCHIVE}" | cut -f1))"
echo
echo "Copy to the target machine and run:"
echo "  scp ${ARCHIVE} user@host:~/"
echo "  ssh user@host 'tar xzf sentin-npu-diag.tar.gz && cd sentin-npu-diag && ./run.sh --power'"
