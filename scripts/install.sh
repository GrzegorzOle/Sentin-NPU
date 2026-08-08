#!/usr/bin/env bash
# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
#
# Install the gateway from a release bundle, and say plainly what this machine can and cannot do.
#
# The script installs; it does not decide. If the NPU driver is missing it says so and carries on,
# because the gateway runs perfectly well on CPU and refusing to install would be a worse answer
# than an honest warning.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-${HOME}/.local/share/sentin-npu}"
BIN_DIR="${BIN_DIR:-${HOME}/.local/bin}"

say()  { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m warning:\033[0m %s\n' "$*"; }

say "installing to ${PREFIX}"
mkdir -p "${PREFIX}" "${BIN_DIR}"
cp -a "${HERE}/lib" "${HERE}/models" "${PREFIX}/"
cp "${HERE}/sentin-gateway" "${HERE}/sentin-doctor" "${PREFIX}/"
if [ -f "${PREFIX}/config.yaml" ]; then
    say "keeping the existing config.yaml"
else
    # model_dir is rewritten to an absolute path. A relative one resolves against the working
    # directory, so the gateway would find its model when started from the install directory and
    # silently fall back to layer 1 from anywhere else.
    sed "s|^  model_dir:.*|  model_dir: ${PREFIX}/models/seq128|" \
        "${HERE}/config.yaml" > "${PREFIX}/config.yaml"
fi

# A wrapper rather than a symlink: the binary needs the bundled OpenVINO on its library path, and
# expecting every user to remember that is a support burden nobody needs.
for tool in sentin-gateway sentin-doctor; do
    cat > "${BIN_DIR}/${tool}" <<EOF
#!/usr/bin/env bash
export LD_LIBRARY_PATH="${PREFIX}/lib:\${LD_LIBRARY_PATH:-}"
exec "${PREFIX}/${tool}" "\$@"
EOF
    chmod +x "${BIN_DIR}/${tool}"
done
say "wrappers in ${BIN_DIR}"

echo
say "checking what this machine offers"

if [ -d /sys/module/intel_vpu ]; then
    version="$(cat /sys/module/intel_vpu/version 2>/dev/null || echo 'version not exposed')"
    say "Intel NPU driver present (intel_vpu, ${version})"
elif [ -d /sys/module/amdxdna ]; then
    warn "this machine has an AMD XDNA NPU, which OpenVINO cannot drive. Inference will use CPU."
else
    warn "no NPU kernel driver found. The gateway will run on CPU."
    echo "         On an Intel Core Ultra, install the NPU driver (intel-npu-driver plus Level Zero)"
    echo "         and re-run this check with: sentin-doctor"
fi

if [ ! -r /sys/class/powercap/intel-rapl:0/energy_uj ] 2>/dev/null; then
    warn "RAPL counters are not readable, so energy measurement is unavailable."
    echo "         Optional, needs root, lasts until reboot:"
    echo "           sudo chmod a+r /sys/class/powercap/intel-rapl:*/energy_uj"
fi

echo
say "devices OpenVINO can actually use"
LD_LIBRARY_PATH="${PREFIX}/lib" "${PREFIX}/sentin-doctor" 2>/dev/null \
    | sed -n '/^OpenVINO/,/^$/p' | sed 's/^/  /' \
    || warn "the diagnostic did not run; try ${BIN_DIR}/sentin-doctor for the reason"

cat <<EOF

Installed. To start:

    sentin-gateway ${PREFIX}/config.yaml

Then point an agent at it:

    export OPENAI_BASE_URL=http://localhost:4141/openai
    export ANTHROPIC_BASE_URL=http://localhost:4141/anthropic

Full device report, including whether the model compiles for your NPU:

    sentin-doctor --model ${PREFIX}/models/seq128/openvino_model.xml --json report.json

To run it as a service, see systemd/sentin-npu.service in this bundle.
EOF
