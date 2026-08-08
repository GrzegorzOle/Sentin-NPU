// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! `sentin-gateway --doctor`: one command that reports everything needed to judge whether this
//! machine can run the NER layer, and on which device.
//!
//! This exists because the project targets hardware its author does not have. Rather than an
//! Intel session being an open-ended debugging expedition, it should be: run one command, send one
//! file. The same command is what a community `npu-report` issue attaches, which makes the people
//! who *do* own Core Ultra machines the project's measurement fleet.
//!
//! Everything here is a fact obtained by trying, not a capability read off a list. A device that
//! enumerates but refuses to compile the model is the single most interesting outcome, so a
//! refusal is recorded and reported rather than treated as an error that aborts the run.

use std::path::{Path, PathBuf};

use sentin_detect::ov;
use serde::Serialize;

use crate::energy;

use crate::fingerprint::Machine;

/// Everything the diagnostic learned about one machine.
///
/// This is what a community `npu-report` issue attaches, so it is written to be readable by
/// someone who has never seen the machine: hardware, drivers, and what OpenVINO did when asked to
/// run the real model on each device.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    /// The gateway build that produced the report.
    pub sentin_version: String,
    /// Hardware, OS and power state — the conditions any number here has to be read against.
    pub machine: Machine,
    /// NPU-capable kernel modules and accelerator device nodes found on the machine.
    pub accelerator_drivers: Vec<DriverInfo>,
    /// The OpenVINO probe, when the runtime could be loaded.
    pub openvino: Option<ov::Report>,
    /// Why the runtime could not be loaded, when it could not. Usually the unversioned-soname
    /// trap, which is why the message spells that out.
    pub openvino_error: Option<String>,
}

/// One kernel-side driver or device node relevant to an NPU.
#[derive(Debug, Serialize)]
pub struct DriverInfo {
    /// Module or node name, e.g. `intel_vpu`, `amdxdna`, `/dev/accel`.
    pub name: String,
    /// Module version where the kernel reports one. The first question on any NPU bug report.
    pub version: Option<String>,
    /// What was found, in words — including "not an OpenVINO target" where that applies.
    pub detail: String,
}

/// Kernel modules that back an NPU, and the accel devices they expose.
///
/// Recorded by name because "which driver, which version" is the first question asked of any NPU
/// bug report, and the answer differs between Intel generations.
fn accelerator_drivers() -> Vec<DriverInfo> {
    let mut found = Vec::new();

    for (module, vendor) in [
        ("intel_vpu", "Intel NPU (VPU/NPU driver)"),
        ("amdxdna", "AMD XDNA NPU — not an OpenVINO target"),
    ] {
        let base = PathBuf::from("/sys/module").join(module);
        if base.exists() {
            found.push(DriverInfo {
                name: module.to_string(),
                version: read_trimmed(base.join("version")),
                detail: vendor.to_string(),
            });
        }
    }

    if let Ok(entries) = std::fs::read_dir("/dev") {
        let accel: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("accel").then_some(name)
            })
            .collect();
        if !accel.is_empty() {
            found.push(DriverInfo {
                name: "/dev/accel".to_string(),
                version: None,
                detail: format!("accelerator device nodes: {}", accel.join(", ")),
            });
        }
    }

    if found.is_empty() {
        found.push(DriverInfo {
            name: "none".to_string(),
            version: None,
            detail: "no NPU kernel module detected".to_string(),
        });
    }
    found
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Run every check and build the report.
#[must_use]
pub fn run(model_xml: Option<&Path>) -> DoctorReport {
    let (openvino, openvino_error) = match ov::probe(model_xml) {
        Ok(report) => (Some(report), None),
        Err(err) => (None, Some(err.to_string())),
    };

    let (backend, domains) = openvino.as_ref().map_or_else(
        || (crate::energy::BACKEND.to_string(), Vec::new()),
        |_| {
            (
                crate::energy::BACKEND.to_string(),
                crate::energy::domains()
                    .iter()
                    .map(|d| d.name.clone())
                    .collect(),
            )
        },
    );

    DoctorReport {
        sentin_version: env!("CARGO_PKG_VERSION").to_string(),
        machine: Machine::detect(&backend, domains),
        accelerator_drivers: accelerator_drivers(),
        openvino,
        openvino_error,
    }
}

/// Print the report for a human. The JSON form is what gets attached to an issue.
pub fn print(report: &DoctorReport) {
    println!("Sentin-NPU doctor (v{})\n", report.sentin_version);

    println!("Machine");
    println!("  {}", report.machine.label());
    println!("  logical CPUs : {}", report.machine.logical_cpus);
    println!("  kernel       : {}", report.machine.kernel);

    println!("\nAccelerator drivers");
    for driver in &report.accelerator_drivers {
        let version = driver.version.as_deref().unwrap_or("version not exposed");
        println!("  {:<12} {:<24} {}", driver.name, version, driver.detail);
    }

    println!("\nOpenVINO");
    let Some(ov) = &report.openvino else {
        println!(
            "  UNAVAILABLE: {}",
            report.openvino_error.as_deref().unwrap_or("unknown error")
        );
        return;
    };

    println!(
        "  runtime      : {}",
        ov.runtime_version.as_deref().unwrap_or("unknown")
    );
    println!("  devices      : {:?}", ov.available_devices);
    println!(
        "  NPU present  : {}",
        if ov.npu_present { "yes" } else { "NO" }
    );
    if let Some(model) = &ov.model_probed {
        println!("  model probed : {model}");
    }

    println!(
        "\n  {:<8}{:>10}{:>12}{:>14}{:>14}  name",
        "device", "compiles", "compile ms", "1st infer ms", "steady ms"
    );
    for device in &ov.devices {
        let compiles = match device.compiles {
            Some(true) => "yes",
            Some(false) => "NO",
            None => "-",
        };
        let fmt = |value: Option<f64>| value.map_or("-".to_string(), |v| format!("{v:.1}"));
        println!(
            "  {:<8}{:>10}{:>12}{:>14}{:>14}  {}",
            device.device,
            compiles,
            fmt(device.compile_ms),
            fmt(device.first_infer_ms),
            fmt(device.steady_infer_ms),
            device.full_name.as_deref().unwrap_or("")
        );
        if let Some(error) = &device.error {
            println!("      error: {error}");
        }
    }

    if !ov.notes.is_empty() {
        println!("\nNotes");
        for note in &ov.notes {
            println!("  * {note}");
        }
    }

    println!(
        "\nRe-run with --json <file> and attach the result to an `npu-report` issue:\n  \
         https://github.com/GrzegorzOle/Sentin-NPU/issues"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_detection_always_returns_something() {
        // Must not panic in a container with no /sys/module and no /dev/accel.
        let drivers = accelerator_drivers();
        assert!(!drivers.is_empty(), "absence must be reported, not omitted");
    }

    #[test]
    fn a_report_is_produced_even_without_openvino() {
        // The whole point is that a machine which cannot run OpenVINO still yields a usable
        // report — that outcome is data, not a crash.
        let report = run(None);
        assert!(!report.sentin_version.is_empty());
        assert!(report.openvino.is_some() || report.openvino_error.is_some());
        let json = serde_json::to_string(&report).expect("report serialises");
        assert!(json.contains("accelerator_drivers"));
    }
}

// ---------------------------------------------------------------------------------------------
// M5 — energy per inference device. The comparison this project exists to produce.
// ---------------------------------------------------------------------------------------------

/// Energy and throughput for one device running the real model.
#[derive(Debug, Clone, Serialize)]
pub struct DevicePower {
    /// The device the model ran on.
    pub device: String,
    /// How many inferences the interval contained.
    pub inferences: u64,
    /// Length of the measured interval.
    pub seconds: f64,
    /// Throughput over the interval.
    pub inferences_per_second: f64,
    /// Mean package power while working. Package-scoped: RAPL has no per-NPU domain, so an NPU
    /// figure is only ever obtained by differencing this against the CPU run.
    pub package_w: f64,
    /// Mean package power doing nothing, measured over the same duration. A laptop draws several
    /// watts idle, which would otherwise swamp the signal entirely.
    pub idle_w: f64,
    /// Package power attributable to the workload, idle removed.
    pub active_w: f64,
    /// The number that makes devices comparable: energy for one inference.
    pub mj_per_inference: f64,
    /// Why this device produced no figure, when it did not.
    pub error: Option<String>,
}

/// The per-device energy comparison: metric M5, and the project's headline result.
#[derive(Debug, Clone, Serialize)]
pub struct PowerReport {
    /// Idle package power, subtracted from every device's figure.
    pub idle_w: f64,
    /// Drift between the idle measured before and after the run. A device difference smaller than
    /// this is noise, and reporting it as a result would be dishonest.
    pub noise_floor_w: f64,
    /// How long each device was measured for.
    pub seconds_per_device: u64,
    /// One entry per device that was tried.
    pub devices: Vec<DevicePower>,
    /// Conditions and caveats that belong with the numbers.
    pub notes: Vec<String>,
}

/// Measure energy per inference on every available device.
///
/// Idle is measured before and after, and the drift between the two is reported as a noise floor:
/// a laptop's package power wanders, and a device whose signal does not clear that drift has not
/// been measured, it has been guessed at.
#[must_use]
pub fn measure_power(model_xml: &Path, seconds_per_device: u64) -> PowerReport {
    use sentin_detect::ov;

    let mut notes = Vec::new();
    let Ok(reader) = energy::Reader::new() else {
        return PowerReport {
            idle_w: 0.0,
            noise_floor_w: 0.0,
            seconds_per_device,
            devices: Vec::new(),
            notes: vec![
                "RAPL counters unreadable — energy cannot be measured. On Linux: \
                 sudo chmod a+r /sys/class/powercap/intel-rapl:*/energy_uj"
                    .to_string(),
            ],
        };
    };

    let idle_first = sample_idle(&reader, seconds_per_device);
    let devices_available = ov::probe(None)
        .map(|r| r.available_devices)
        .unwrap_or_default();

    let mut devices = Vec::new();
    for device in &devices_available {
        let start = reader.sample();
        let outcome = ov::run_for(
            model_xml,
            device,
            std::time::Duration::from_secs(seconds_per_device),
        );
        let end = reader.sample();
        let elapsed = energy::elapsed(&start, &end);
        let joules = package_joules(&reader, &start, &end);

        match outcome {
            Ok((count, _)) => {
                let seconds = elapsed.as_secs_f64().max(f64::EPSILON);
                let package_w = joules / seconds;
                let active_w = (package_w - idle_first).max(0.0);
                devices.push(DevicePower {
                    device: device.clone(),
                    inferences: count,
                    seconds,
                    inferences_per_second: count as f64 / seconds,
                    package_w,
                    idle_w: idle_first,
                    active_w,
                    mj_per_inference: if count > 0 {
                        active_w * seconds * 1000.0 / count as f64
                    } else {
                        0.0
                    },
                    error: None,
                });
            }
            Err(err) => {
                notes.push(format!("{device} could not run the model: {err}"));
                devices.push(DevicePower {
                    device: device.clone(),
                    inferences: 0,
                    seconds: 0.0,
                    inferences_per_second: 0.0,
                    package_w: 0.0,
                    idle_w: idle_first,
                    active_w: 0.0,
                    mj_per_inference: 0.0,
                    error: Some(err),
                });
            }
        }
    }

    let idle_last = sample_idle(&reader, seconds_per_device.min(10));
    let noise_floor_w = (idle_first - idle_last).abs();
    notes.push(format!(
        "Idle measured before ({idle_first:.2} W) and after ({idle_last:.2} W); the difference is \
         the noise floor a device's signal has to clear."
    ));
    notes.push(
        "Package RAPL covers the whole SoC, including the NPU — there is no separate NPU domain. \
         These figures are therefore package power *while* a device was working, and the NPU's own \
         draw is the difference between its row and the CPU row, not an absolute reading."
            .to_string(),
    );

    PowerReport {
        idle_w: idle_first,
        noise_floor_w,
        seconds_per_device,
        devices,
        notes,
    }
}

fn sample_idle(reader: &energy::Reader, seconds: u64) -> f64 {
    std::thread::sleep(std::time::Duration::from_secs(2));
    let start = reader.sample();
    std::thread::sleep(std::time::Duration::from_secs(seconds));
    let end = reader.sample();
    let elapsed = energy::elapsed(&start, &end)
        .as_secs_f64()
        .max(f64::EPSILON);
    package_joules(reader, &start, &end) / elapsed
}

fn package_joules(reader: &energy::Reader, start: &energy::Sample, end: &energy::Sample) -> f64 {
    reader
        .delta_uj(start, end)
        .into_iter()
        .find(|(name, _)| name.starts_with("package"))
        .map_or(0.0, |(_, uj)| uj as f64 / 1_000_000.0)
}

/// Print the power comparison for a human.
pub fn print_power(report: &PowerReport) {
    println!("\nEnergy per inference device (M5)");
    println!(
        "  idle {:.2} W, noise floor {:.2} W, {} s per device\n",
        report.idle_w, report.noise_floor_w, report.seconds_per_device
    );
    println!(
        "  {:<8}{:>12}{:>10}{:>12}{:>12}{:>14}",
        "device", "inferences", "per s", "package W", "active W", "mJ/inference"
    );
    for device in &report.devices {
        if let Some(error) = &device.error {
            println!("  {:<8}  refused: {error}", device.device);
            continue;
        }
        println!(
            "  {:<8}{:>12}{:>10.1}{:>12.2}{:>12.2}{:>14.2}",
            device.device,
            device.inferences,
            device.inferences_per_second,
            device.package_w,
            device.active_w,
            device.mj_per_inference
        );
    }
    for note in &report.notes {
        println!("\n  * {note}");
    }
}
