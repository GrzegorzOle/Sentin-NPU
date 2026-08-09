#!/usr/bin/env bash
# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
#
# Run every diagnostic and collect the output into one archive.
#
# Logging is deliberately exhaustive. This runs on a machine we cannot inspect interactively, so
# anything not captured here is a question that costs another round trip — and access to Intel NPU
# hardware is the scarce resource this whole project is arranged around. Environment, kernel
# modules, device nodes and full command output all go into the results, not just the numbers.

set -uo pipefail   # deliberately not -e: a failing probe is data, and must not stop the run

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESULTS="${HERE}/results"
LOG="${RESULTS}/full.log"
WITH_POWER=0
[ "${1:-}" = "--power" ] && WITH_POWER=1

rm -rf "${RESULTS}"; mkdir -p "${RESULTS}"
exec > >(tee -a "${LOG}") 2>&1

section() { printf '\n\033[1m===== %s =====\033[0m\n' "$*"; }
run() { printf '\n$ %s\n' "$*"; eval "$@" 2>&1 || printf '(exit %s — recorded, continuing)\n' "$?"; }

section "when and where"
run "date -Is"
run "id"
run "uname -a"
run "cat /etc/os-release 2>/dev/null | head -4"
run "ldd --version | head -1"

section "cpu and memory"
run "lscpu | head -25"
run "free -h"

section "npu: kernel side"
# The first question anyone asks about an NPU bug is which driver, and which version.
run "lsmod | grep -iE 'vpu|npu|xdna|accel' || echo '(no matching kernel module)'"
run "modinfo intel_vpu 2>/dev/null | head -8 || echo '(intel_vpu not present)'"
run "ls -l /dev/accel/ 2>/dev/null || echo '(no /dev/accel)'"
run "lspci -nn 2>/dev/null | grep -iE 'neural|npu|vpu|ai boost' || echo '(nothing matching in lspci)'"
run "dmesg 2>/dev/null | grep -iE 'intel_vpu|ivpu|npu' | tail -20 || echo '(no dmesg access or no matches)'"

section "permissions that matter"
run "ls -l /dev/accel/* 2>/dev/null"
run "ls -l /sys/class/powercap/intel-rapl:*/energy_uj 2>/dev/null || echo '(no RAPL)'"
run "cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo '(no cpufreq)'"
run "cat /sys/firmware/acpi/platform_profile 2>/dev/null || echo '(no platform profile)'"
run "cat /sys/class/power_supply/A*/online 2>/dev/null | head -1 || echo '(no mains supply reported)'"

section "bundle contents"
run "ls -l '${HERE}/sentin-doctor'"
run "ls '${HERE}/lib' | head -20"
run "ls -R '${HERE}/models' | head -20"

export LD_LIBRARY_PATH="${HERE}/lib:${LD_LIBRARY_PATH:-}"
MODEL="${HERE}/models/seq128/openvino_model.xml"

section "device report (seq 128)"
if [ -f "${MODEL}" ]; then
    run "'${HERE}/sentin-doctor' --model '${MODEL}' --json '${RESULTS}/doctor-seq128.json'"
else
    echo "no model in the bundle; enumeration only"
    run "'${HERE}/sentin-doctor' --json '${RESULTS}/doctor-nomodel.json'"
fi

MODEL512="${HERE}/models/seq512/openvino_model.xml"
if [ -f "${MODEL512}" ]; then
    section "device report (seq 512)"
    # Both shape variants are tested because an NPU may accept one and refuse the other, and
    # which one it refuses is exactly the sort of thing this project exists to find out.
    run "'${HERE}/sentin-doctor' --model '${MODEL512}' --json '${RESULTS}/doctor-seq512.json'"
fi

# M2b — the latency a request actually pays, per device. sentin-doctor times the inference alone;
# this times the whole pipeline, which is the metric Phase 5 has to report next to the power table.
#
# All three devices are attempted rather than only the ones enumerated, and deliberately so: the
# harness prints the device it actually ran on, so an absent or refusing NPU shows up as a recorded
# attempt that fell back, which is the result this project exists to collect. Parsing the device
# list out of the JSON first would need jq, which a clean test machine may not have.
section "pipeline latency per device (M2b)"
if [ -f "${HERE}/sentin-bench" ] && [ -f "${MODEL}" ]; then
    for dev in NPU GPU CPU; do
        run "'${HERE}/sentin-bench' --device ${dev} --model-dir '${HERE}/models/seq128' \
             --m2b-only --json '${RESULTS}/bench-m2b-${dev}.json'"
    done
else
    echo "(no sentin-bench in this bundle, or no model — M2b not measured)"
fi

if [ "${WITH_POWER}" = "1" ] && [ -f "${MODEL}" ]; then
    section "energy per device"
    # Repeats are what make this metric admissible: the device differences it reports are the same
    # size as a laptop package's own drift, so a single pass cannot tell them apart. Five measured
    # rounds plus a discarded warm-up, at three load levels, takes roughly fifteen minutes.
    if [ -r /sys/class/powercap/intel-rapl:0/energy_uj ]; then
        run "'${HERE}/sentin-doctor' --model '${MODEL}' --power --power-seconds 15 \
             --power-repeats 5 --power-json '${RESULTS}/power.json'"
    else
        echo "RAPL counters are not readable, so energy cannot be measured."
        echo "To enable (needs root, lasts until reboot):"
        echo "  sudo chmod a+r /sys/class/powercap/intel-rapl:*/energy_uj"
        echo "Then re-run: ./run.sh --power"
    fi
fi

section "collecting"
ARCHIVE="${HERE}/sentin-npu-results-$(hostname -s 2>/dev/null || echo host)-$(date +%Y%m%d-%H%M).tar.gz"
tar -czf "${ARCHIVE}" -C "${HERE}" results
echo
echo "Everything is in: ${ARCHIVE}"
echo "Send that one file back. It contains hardware, driver and timing information;"
echo "it carries no personal data and nothing from any inspected request."
