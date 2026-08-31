#!/usr/bin/env bash
# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
#
# Build the release bundles for Linux and Windows.
#
# Each bundle is self-contained: binary, OpenVINO runtime, both model variants and a script.
# The target is a plain client workstation with nothing installed and possibly no network.
#
# Both a release and a debug binary ship in each bundle, deliberately. A debug build is 10-50x
# slower at inference, which would make the latency and energy figures — the entire reason for
# going to that machine — meaningless. So measurements use the release binary and the debug one is
# there for when something crashes and a backtrace matters more than a number.

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${REPO}/dist"
VERSION="${1:-0.0.0.1}"
OV_LINUX="${REPO}/tools/.venv/lib/python3.11/site-packages/openvino/libs"
OV_WINDOWS="${OV_WINDOWS_LIBS:-}"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

build_linux() {
    say "linux: building against an old glibc (container)"
    for profile in release debug; do
        local flag=""
        [ "${profile}" = "release" ] && flag="--release"
        docker run --rm -v "${REPO}":/w:z -w /w/gateway \
            -e CARGO_TARGET_DIR=/w/gateway/target-bullseye rust:1-bullseye \
            cargo build ${flag} -p sentin-diag --bin sentin-doctor
    done
    # The gateway itself, so a release is something you can run and not only diagnose, and the
    # bench harness, without which M2b cannot be measured on a machine that has no Rust toolchain
    # -- which is every test machine. sentin-doctor times the inference; only this times the
    # pipeline a request actually goes through.
    docker run --rm -v "${REPO}":/w:z -w /w/gateway \
        -e CARGO_TARGET_DIR=/w/gateway/target-bullseye rust:1-bullseye \
        cargo build --release -p sentin-proxy --bin sentin-gateway --bin sentin-bench
}

build_windows() {
    say "windows: cross-compiling with mingw (container)"
    docker build -q -t sentin-winbuild - <<'DOCKERFILE' >/dev/null
FROM rust:1-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends mingw-w64 \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-pc-windows-gnu
DOCKERFILE
    for profile in release debug; do
        local flag=""
        [ "${profile}" = "release" ] && flag="--release"
        docker run --rm -v "${REPO}":/w:z -w /w/gateway \
            -e CARGO_TARGET_DIR=/w/gateway/target-win sentin-winbuild \
            cargo build ${flag} --target x86_64-pc-windows-gnu -p sentin-diag --bin sentin-doctor
    done
    # The gateway and the harness cross-compile too. This was assumed to be blocked -- the
    # expectation was aws-lc-sys wanting a C toolchain -- and was simply never tried; reqwest
    # resolves to rustls here, so mingw builds all three. Without sentin-bench a Windows tester
    # cannot measure M2 at all, which is the same gap that made the Linux bundle useless for half
    # of Phase 5's exit criteria.
    docker run --rm -v "${REPO}":/w:z -w /w/gateway \
        -e CARGO_TARGET_DIR=/w/gateway/target-win sentin-winbuild \
        cargo build --release --target x86_64-pc-windows-gnu \
        -p sentin-proxy --bin sentin-gateway --bin sentin-bench
}

copy_models() {
    local dest="$1"
    for seq in 128 512; do
        local src="${REPO}/models/herbert/int8/seq${seq}"
        [ -d "${src}" ] || { echo "WARNING: no model at ${src}" >&2; continue; }
        mkdir -p "${dest}/seq${seq}"
        # The tokenizer travels too. The diagnostic only compiles and runs the graph so it never
        # needed one, which is exactly why its absence went unnoticed until the gateway itself was
        # installed from a bundle and layer 2 refused to start.
        # `if` rather than `[ -f ] && cp`: under `set -e` the short-circuit form aborts the whole
        # script the first time an optional file is absent, which is how an earlier version
        # silently stopped packing and left a stale archive behind.
        for f in openvino_model.xml openvino_model.bin config.json tokenizer.json \
                 tokenizer_config.json special_tokens_map.json vocab.json merges.txt; do
            if [ -f "${src}/${f}" ]; then
                cp "${src}/${f}" "${dest}/seq${seq}/"
            fi
        done
    done
}

write_readme() {
    local dir="$1" platform="$2" cmd="$3" dbg="$4"
    cat > "${dir}/README.txt" <<EOF
Sentin-NPU diagnostic bundle ${VERSION} (test build) — ${platform}
=================================================================

Self-contained. Nothing needs installing: no Rust, no Python, no OpenVINO, no network.

    ${cmd}

Results are written to results/ and collected into a single archive to send back.

The diagnostic ships twice. Measurements use the optimised build; ${dbg} runs a debug
build with full backtraces, for when something crashes and a number matters less than
knowing why.

If the NPU does not appear, the report says so and why. That outcome is a result, not a
failure — please send it either way. It contains hardware, driver and timing information;
no personal data and nothing from any inspected request.
EOF
}

stage_wazuh() {
    local stage="$1"
    mkdir -p "${stage}/wazuh"
    cp "${REPO}/packaging/wazuh/sentin_npu_rules.xml" \
       "${REPO}/packaging/wazuh/agent-localfile.conf" \
       "${REPO}/packaging/wazuh/sentin-npu-dashboard.ndjson" \
       "${REPO}/packaging/wazuh/deploy-manager.sh" \
       "${REPO}/packaging/wazuh/README.md" "${stage}/wazuh/"
    chmod +x "${stage}/wazuh/deploy-manager.sh"
}

stage_linux() {
    local stage="${OUT}/sentin-npu-diag-${VERSION}-linux-x64"
    say "linux: staging"
    rm -rf "${stage}"; mkdir -p "${stage}/lib" "${stage}/models"
    cp "${REPO}/gateway/target-bullseye/release/sentin-doctor" "${stage}/sentin-doctor"
    cp "${REPO}/gateway/target-bullseye/debug/sentin-doctor" "${stage}/sentin-doctor-debug"

    [ -d "${OV_LINUX}" ] || { echo "no OpenVINO libs at ${OV_LINUX}" >&2; exit 1; }
    cp -a "${OV_LINUX}"/. "${stage}/lib/"
    # dlopen looks for unversioned sonames; the Python wheel ships only versioned ones.
    ( cd "${stage}/lib"; for f in *.so.*; do [ -e "$f" ] && ln -sf "$f" "${f%%.so.*}.so"; done ) || true

    cp "${REPO}/gateway/target-bullseye/release/sentin-gateway" "${stage}/sentin-gateway"
    cp "${REPO}/gateway/target-bullseye/release/sentin-bench" "${stage}/sentin-bench"
    mkdir -p "${stage}/systemd"
    cp "${REPO}/packaging/systemd/sentin-npu.service" "${stage}/systemd/"
    cp "${REPO}/scripts/install.sh" "${stage}/install.sh"
    # The SIEM integration travels with the binary. Someone deploying the gateway into a SOC has a
    # Wazuh administrator to hand the rules to on the same day, and asking them to go and find a
    # directory in a git repository is how an integration stays unshipped.
    stage_wazuh "${stage}"
    # The shipped config points layer 2 at the bundled model rather than a path that only exists
    # in the source tree, so the gateway works straight out of the archive.
    sed -e 's|^  model_dir:.*|  model_dir: models/seq128|' \
        -e 's|^  device: AUTO|  device: AUTO|' \
        "${REPO}/config/default.yaml" > "${stage}/config.yaml"

    copy_models "${stage}/models"
    cp "${REPO}/scripts/run-diagnostics.sh" "${stage}/run.sh"
    chmod +x "${stage}/run.sh" "${stage}/install.sh" "${stage}/sentin-doctor" \
             "${stage}/sentin-doctor-debug" "${stage}/sentin-gateway" "${stage}/sentin-bench"
    write_readme "${stage}" "Linux x86-64 (glibc 2.30+)" \
        "./run.sh              # device report
    ./run.sh --power      # also energy per device" "sentin-doctor-debug"

    say "linux: packing"
    tar -czf "${OUT}/sentin-npu-diag-${VERSION}-linux-x64.tar.gz" -C "${OUT}" \
        "$(basename "${stage}")"
}

stage_windows() {
    local stage="${OUT}/sentin-npu-diag-${VERSION}-windows-x64"
    say "windows: staging"
    rm -rf "${stage}"; mkdir -p "${stage}/lib" "${stage}/models"
    cp "${REPO}/gateway/target-win/x86_64-pc-windows-gnu/release/sentin-doctor.exe" \
       "${stage}/sentin-doctor.exe"
    cp "${REPO}/gateway/target-win/x86_64-pc-windows-gnu/debug/sentin-doctor.exe" \
       "${stage}/sentin-doctor-debug.exe"

    cp "${REPO}/gateway/target-win/x86_64-pc-windows-gnu/release/sentin-gateway.exe" \
       "${stage}/sentin-gateway.exe"
    cp "${REPO}/gateway/target-win/x86_64-pc-windows-gnu/release/sentin-bench.exe" \
       "${stage}/sentin-bench.exe"

    if [ -n "${OV_WINDOWS}" ] && [ -d "${OV_WINDOWS}" ]; then
        cp -a "${OV_WINDOWS}"/*.dll "${stage}/lib/"
    else
        echo "WARNING: OV_WINDOWS_LIBS not set — Windows bundle has no OpenVINO runtime" >&2
    fi

    # Same rewrite as Linux: a config pointing into the source tree silently drops the bundle to
    # layer 1, and the warning is easy to miss in a startup log.
    sed -e 's|^  model_dir:.*|  model_dir: models/seq128|' \
        "${REPO}/config/default.yaml" > "${stage}/config.yaml"

    stage_wazuh "${stage}"
    copy_models "${stage}/models"
    cp "${REPO}/scripts/run-diagnostics.ps1" "${stage}/run.ps1"
    write_readme "${stage}" "Windows x86-64" \
        "powershell -ExecutionPolicy Bypass -File run.ps1
    powershell -ExecutionPolicy Bypass -File run.ps1 -Power" "run.ps1 -Debug"

    say "windows: packing"
    ( cd "${OUT}" && zip -qr "sentin-npu-diag-${VERSION}-windows-x64.zip" \
        "$(basename "${stage}")" )
}

mkdir -p "${OUT}"
build_linux
build_windows
stage_linux
stage_windows
# The IR also ships on its own, for people who want the model without a bundle around it. The
# bundles keep their embedded copy regardless — they have to work with no network.
"${REPO}/scripts/make-model-assets.sh" "${VERSION}"

say "artefacts:"
ls -lh "${OUT}"/*.tar.gz "${OUT}"/*.zip 2>/dev/null | awk '{printf "  %-10s %s\n", $5, $NF}'
