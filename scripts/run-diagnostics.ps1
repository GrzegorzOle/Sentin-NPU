# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
#
# Run every diagnostic on Windows and collect the output into one archive.
#
# Logging is deliberately exhaustive: this runs on a machine nobody can inspect interactively, so
# anything not captured here costs another round trip on hardware that is hard to get hold of.

param(
    [switch]$Power,
    [switch]$Debug
)

$ErrorActionPreference = 'Continue'   # a failing probe is data, and must not stop the run
$Here = Split-Path -Parent $MyInvocation.MyCommand.Path
$Results = Join-Path $Here 'results'
Remove-Item -Recurse -Force $Results -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Results | Out-Null
$Log = Join-Path $Results 'full.log'

function Section($name) {
    $line = "`n===== $name ====="
    Write-Host $line
    Add-Content -Path $Log -Value $line
}

function Run($description, [scriptblock]$block) {
    Write-Host "`n> $description"
    Add-Content -Path $Log -Value "`n> $description"
    try {
        # The OpenCL kernel compiler on the NVIDIA path writes "N warnings generated." to the
        # process output, once per compilation unit. It says nothing about this machine and it
        # appeared thirteen times in the first Windows run, so it is dropped rather than collected.
        $out = & $block 2>&1 |
            Where-Object { "$_" -notmatch '^\s*\d+ warnings generated\.\s*$' } |
            Out-String
    } catch {
        $out = "(failed: $_ - recorded, continuing)"
    }
    Write-Host $out
    Add-Content -Path $Log -Value $out
}

Section 'when and where'
Run 'date' { Get-Date -Format o }
Run 'whoami' { whoami }
Run 'os' { Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber | Format-List }

Section 'cpu and memory'
Run 'cpu' { Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors | Format-List }
Run 'memory' { Get-CimInstance Win32_ComputerSystem | Select-Object TotalPhysicalMemory | Format-List }

Section 'npu: driver side'
# The first question asked of any NPU report is which driver and which version.
Run 'neural processors' {
    # Word boundaries matter here: an unanchored 'NPU' matches the middle of "USB I-npu-t Device",
    # which is how the first Windows run reported eight USB keyboards as neural processors.
    Get-CimInstance Win32_PnPSignedDriver |
        Where-Object { $_.DeviceName -match '(?i)\b(NPU|VPU)\b|(?i)(neural|ai boost)' } |
        Select-Object DeviceName, DriverVersion, DriverDate, Manufacturer | Format-List
}
Run 'all accelerator-ish devices' {
    Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue |
        Where-Object {
            $_.Class -match 'ComputeAccelerator|System' -and
            $_.FriendlyName -match '(?i)\b(NPU|VPU|AI)\b|(?i)(neural|ai boost)'
        } |
        Select-Object FriendlyName, Status, Class | Format-Table -AutoSize
}
Run 'display adapters (for the iGPU comparison)' {
    Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion | Format-List
}

Section 'bundle contents'
Run 'files' { Get-ChildItem $Here | Select-Object Name, Length | Format-Table -AutoSize }
Run 'libs' { Get-ChildItem (Join-Path $Here 'lib') -Filter *.dll | Select-Object -First 20 Name | Format-Table -AutoSize }

# OpenVINO is loaded from the bundled lib directory; nothing needs installing.
$env:PATH = (Join-Path $Here 'lib') + ';' + $env:PATH
$env:RUST_BACKTRACE = 'full'

$Exe = Join-Path $Here $(if ($Debug) { 'sentin-doctor-debug.exe' } else { 'sentin-doctor.exe' })
$Model128 = Join-Path $Here 'models\seq128\openvino_model.xml'
$Model512 = Join-Path $Here 'models\seq512\openvino_model.xml'

Section 'device report (seq 128)'
if (Test-Path $Model128) {
    Run 'doctor seq128' { & $Exe --model $Model128 --json (Join-Path $Results 'doctor-seq128.json') }
} else {
    Run 'doctor (enumeration only)' { & $Exe --json (Join-Path $Results 'doctor-nomodel.json') }
}

if (Test-Path $Model512) {
    Section 'device report (seq 512)'
    # Both shape variants: an NPU may accept one and refuse the other, and which one is exactly
    # the sort of thing this project exists to find out.
    Run 'doctor seq512' { & $Exe --model $Model512 --json (Join-Path $Results 'doctor-seq512.json') }
}

# M2b -- the latency a request actually pays, per device. The doctor times the inference alone;
# this times the whole pipeline, which is the number that belongs beside it.
#
# All three devices are attempted rather than only the enumerated ones, deliberately: the harness
# prints the device that actually ran, so an absent or refusing NPU shows up as a recorded attempt
# that fell back. That is the result worth collecting.
Section 'pipeline latency per device (M2b)'
$Bench = Join-Path $Here 'sentin-bench.exe'
if ((Test-Path $Bench) -and (Test-Path $Model128)) {
    foreach ($Dev in @('NPU', 'GPU', 'CPU')) {
        Run "bench m2b $Dev" {
            & $Bench --device $Dev --model-dir (Join-Path $Here 'models\seq128') `
                     --m2b-only --json (Join-Path $Results "bench-m2b-$Dev.json")
        }
    }
} else {
    Write-Host '(no sentin-bench.exe in this bundle, or no model - M2b not measured)'
}

if ($Power) {
    Section 'energy per device'
    # Windows has no powercap sysfs; RAPL is reachable only through a signed kernel driver, so the
    # doctor reports energy as unsupported here. Intel PCM is the fallback, and its numbers are a
    # different measurement that must not be put in the same column as Linux RAPL figures.
    Run 'power (expected: unsupported on Windows)' { & $Exe --model $Model128 --power --power-repeats 1 }
}

Section 'collecting'
$Stamp = Get-Date -Format 'yyyyMMdd-HHmm'
$Archive = Join-Path $Here "sentin-npu-results-$env:COMPUTERNAME-$Stamp.zip"
Compress-Archive -Path $Results -DestinationPath $Archive -Force
Write-Host "`nEverything is in: $Archive"
Write-Host 'Send that one file back. It contains hardware, driver and timing information;'
Write-Host 'it carries no personal data and nothing from any inspected request.'
if (-not $Debug) {
    Write-Host "`nIf something crashed, re-run with -Debug for a slower build with full backtraces."
}
