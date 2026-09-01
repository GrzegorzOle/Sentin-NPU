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
VERSION="${1:-0.2.1}"
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
    # The platform string is prose ("Linux x86-64 (glibc 2.30+)"); the docs filename is not.
    local platform_slug=linux
    case "${platform}" in Windows*) platform_slug=windows ;; esac
    cat > "${dir}/README.txt" <<EOF
Sentin-NPU ${VERSION} (test build) - ${platform}
================================================

Self-contained. Nothing needs installing: no Rust, no Python, no OpenVINO, no network.
The gateway, the diagnostics, the OpenVINO runtime, the quantized model and the Wazuh
integration are all in this directory.


WHAT IS IN HERE
---------------

  docs/            everything below is documented there, in full
  wazuh/           rules, dashboard and the guide for a Wazuh administrator
  config.yaml      the gateway's configuration, pointing at the bundled model
  models/          the quantized IR and its tokenizer
  lib/             the OpenVINO runtime


FIRST: WHAT CAN THIS MACHINE DO
-------------------------------

    ${cmd}

Results are written to results/ and collected into a single archive to send back.
It contains hardware, driver and timing information; no personal data and nothing
from any inspected request.

The diagnostic ships twice. Measurements use the optimised build; ${dbg} runs a debug
build with full backtraces, for when something crashes and a number matters less than
knowing why.

If the NPU does not appear, the report says so and why. That outcome is a result, not
a failure - please send it either way.


THEN: RUNNING THE GATEWAY
-------------------------

Edit config.yaml, then start it. Point your client at http://<host>:<port>/openai,
/anthropic or /google instead of the provider's own address.

The one setting whose wrong value is silent is inference.model_dir. It ships relative
to this directory, so the gateway must be started from here; make it an absolute path
before running it from anywhere else, or layer 2 goes missing with only a warning.
A healthy start logs "layer 2 ready device=..."; anything else means the checksum
detectors are working and the NER model is not.

An installer that does all of this for you, and registers a service, is published
alongside this archive on the releases page. See docs/install-${platform_slug}.md.


AND: THE AUDIT TRAIL
--------------------

Every detection produces a metadata-only event - the data type, the verdict, the
caller, the model the data was heading for, and whether the identifier was typed
into the prompt or found inside an attached file. Never the text itself.
docs/events.md is the authoritative field reference.

Attachments are decoded and read: PDF, Office and OpenDocument files, and plain
text in UTF-8, UTF-16 or a single-byte code page. One that cannot be read - an
image, an archive, something encrypted - is reported rather than passed over in
silence. There is no OCR.

To get those events into Wazuh, hand wazuh/ to whoever runs it: rules, the agent
collection snippet, a fifteen-panel dashboard and a deployment guide written for
someone who has never seen this project. No decoder to write.


DOCUMENTATION
-------------

  docs/install-${platform_slug}.md   installing, configuring, running as a service
  docs/events.md                     the audit event schema, binding
  docs/overview.md                   what this is and why it is built this way
  docs/benchmarks.md                 every measurement, with the hardware it came from
  docs/npu-compat.md                 per-device compatibility reports
  wazuh/README.md                    the SIEM integration, end to end
  docs/LICENSE, docs/NOTICE.md       Apache 2.0, and the model's own licence

https://github.com/GrzegorzOle/Sentin-NPU
EOF
}

# The documentation travels with the software. Someone who downloads a 280 MB archive should not
# have to go and find a git repository to learn how to configure it, and the Wazuh administrator
# they hand the rules to should get the event schema in the same directory as the rules.
stage_docs() {
    local stage="$1" platform="$2"
    mkdir -p "${stage}/docs"
    cp "${REPO}/README.md"        "${stage}/docs/overview.md"
    cp "${REPO}/docs/events.md"   "${stage}/docs/events.md"
    cp "${REPO}/docs/benchmarks.md" "${stage}/docs/benchmarks.md"
    cp "${REPO}/docs/npu-compat.md" "${stage}/docs/npu-compat.md"
    cp "${REPO}/LICENSE"          "${stage}/docs/LICENSE"
    cp "${REPO}/NOTICE.md"        "${stage}/docs/NOTICE.md"
    case "${platform}" in
        windows) cp "${REPO}/packaging/windows/README.md" "${stage}/docs/install-windows.md" ;;
        linux)   cp "${REPO}/packaging/linux/README.md"   "${stage}/docs/install-linux.md" ;;
    esac
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
    stage_docs "${stage}" linux
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
        echo "WARNING: OV_WINDOWS_LIBS not set - Windows bundle has no OpenVINO runtime" >&2
    fi

    # Same rewrite as Linux: a config pointing into the source tree silently drops the bundle to
    # layer 1, and the warning is easy to miss in a startup log.
    sed -e 's|^  model_dir:.*|  model_dir: models/seq128|' \
        "${REPO}/config/default.yaml" > "${stage}/config.yaml"

    stage_wazuh "${stage}"
    stage_docs "${stage}" windows
    copy_models "${stage}/models"
    cp "${REPO}/scripts/run-diagnostics.ps1" "${stage}/run.ps1"
    write_readme "${stage}" "Windows x86-64" \
        "powershell -ExecutionPolicy Bypass -File run.ps1
    powershell -ExecutionPolicy Bypass -File run.ps1 -Power" "run.ps1 -Debug"

    say "windows: packing"
    ( cd "${OUT}" && zip -qr "sentin-npu-diag-${VERSION}-windows-x64.zip" \
        "$(basename "${stage}")" )
}

# The documentation on its own, for anyone who wants to read before downloading 280 MB, and for
# the Wazuh administrator who is not the person deploying the gateway and has no use for the
# binaries at all.
stage_docs_archive() {
    local dir="${OUT}/sentin-npu-docs-${VERSION}"
    say "docs: staging"
    rm -rf "${dir}"; mkdir -p "${dir}"
    stage_docs "${dir}" all
    cp "${REPO}/packaging/windows/README.md" "${dir}/docs/install-windows.md"
    cp "${REPO}/packaging/linux/README.md"   "${dir}/docs/install-linux.md"
    stage_wazuh "${dir}"
    mv "${dir}/docs"/* "${dir}/" && rmdir "${dir}/docs"
    ( cd "${OUT}" && zip -qr "sentin-npu-docs-${VERSION}.zip" "$(basename "${dir}")" )
    rm -rf "${dir}"
}

mkdir -p "${OUT}"
build_linux
build_windows
stage_linux
stage_windows
# The IR also ships on its own, for people who want the model without a bundle around it. The
# bundles keep their embedded copy regardless — they have to work with no network.
"${REPO}/scripts/make-model-assets.sh" "${VERSION}"

stage_docs_archive

say "artefacts:"
ls -lh "${OUT}"/*.tar.gz "${OUT}"/*.zip 2>/dev/null | awk '{printf "  %-10s %s\n", $5, $NF}'
