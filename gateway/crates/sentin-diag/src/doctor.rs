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

use std::collections::BTreeMap;
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
    fn median_and_p95_summarise_repeats_without_panicking_on_nothing() {
        // A refused device contributes no trials, and the summary runs over it anyway.
        assert!((median(&mut []) - 0.0).abs() < f64::EPSILON);
        assert!((p95(&mut []) - 0.0).abs() < f64::EPSILON);

        let mut five = [4.0, 1.0, 5.0, 2.0, 3.0];
        assert!((median(&mut five) - 3.0).abs() < f64::EPSILON);
        // Nearest rank over five samples puts p95 at the maximum. That is the honest reading of a
        // p95 taken from five points, which is why the report prints the spread beside it.
        assert!((p95(&mut five) - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn every_power_scenario_is_either_saturation_or_a_positive_rate() {
        // A zero or negative rate would make the paced loop divide by zero and the label lie.
        for (label, rps) in POWER_SCENARIOS {
            assert!(!label.is_empty());
            if let Some(rate) = rps {
                assert!(rate > 0.0, "{label} has a non-positive rate");
            }
        }
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
    /// Which load this row was taken under: `saturation`, or a fixed rate such as `10 rps`.
    pub scenario: String,
    /// The requested rate, absent for saturation. Kept alongside the label so a reader parsing the
    /// JSON does not have to interpret prose.
    pub target_rps: Option<f64>,
    /// How many measured repeats this row summarises, after the discarded warm-up.
    pub repeats: usize,
    /// Median inferences per second across the repeats.
    pub inferences_per_second: f64,
    /// Median mean-package-power while working. Package-scoped: RAPL has no per-NPU domain, so an
    /// NPU figure is only ever obtained by differencing this against the CPU run.
    pub package_w: f64,
    /// Median package power doing nothing. A laptop draws several watts idle, which would
    /// otherwise swamp the signal entirely.
    pub idle_w: f64,
    /// Median package power attributable to the workload, idle removed.
    pub active_w: f64,
    /// p95 of the same quantity, so a reader can see how stable it was.
    pub active_w_p95: f64,
    /// Median energy for one inference — the number that makes devices comparable.
    pub mj_per_inference: f64,
    /// p95 of energy per inference.
    pub mj_per_inference_p95: f64,
    /// Spread of `active_w` across repeats (max − min). A difference between two devices smaller
    /// than this has not been measured.
    pub active_w_spread: f64,
    /// Whether the workload's own draw cleared the idle noise floor. When false, every figure in
    /// this row is an upper bound rather than a measurement, and must be read as one.
    pub above_noise: bool,
    /// Every individual repeat, so the summary can be checked rather than trusted.
    pub trials: Vec<PowerTrial>,
    /// Why this device produced no figure, when it did not.
    pub error: Option<String>,
}

/// One measured repeat, kept so the medians above can be audited.
#[derive(Debug, Clone, Serialize)]
pub struct PowerTrial {
    /// Inferences completed in this repeat.
    pub inferences: u64,
    /// Length of this repeat.
    pub seconds: f64,
    /// Mean package power over this repeat.
    pub package_w: f64,
    /// Package power above the idle sampled in the same round.
    pub active_w: f64,
    /// Energy per inference implied by this repeat alone.
    pub mj_per_inference: f64,
}

/// The per-device energy comparison: metric M5, and the project's headline result.
#[derive(Debug, Clone, Serialize)]
pub struct PowerReport {
    /// Median idle package power, subtracted from every device's figure.
    pub idle_w: f64,
    /// Spread of the idle samples taken between rounds (max − min). A device difference smaller
    /// than this is noise, and reporting it as a result would be dishonest.
    pub noise_floor_w: f64,
    /// How long each measurement ran for.
    pub seconds_per_device: u64,
    /// Measured repeats per row, after the discarded warm-up round.
    pub repeats: usize,
    /// Every idle sample, one per round including the warm-up.
    pub idle_samples_w: Vec<f64>,
    /// One entry per device and load scenario.
    pub devices: Vec<DevicePower>,
    /// Conditions and caveats that belong with the numbers.
    pub notes: Vec<String>,
}

/// Load scenarios M5 is taken under: saturation, and the rates a real deployment sees.
const POWER_SCENARIOS: [(&str, Option<f64>); 3] = [
    ("saturation", None),
    ("10 rps", Some(10.0)),
    ("1 rps", Some(1.0)),
];

/// Median of a slice, which is empty-safe because a refused device has no trials.
fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values[values.len() / 2]
}

/// p95 by nearest rank. With five repeats this is the maximum, which is the honest reading of a
/// p95 taken from five samples and is why the spread is reported next to it.
fn p95(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((values.len() as f64) * 0.95).ceil() as usize;
    values[rank.saturating_sub(1).min(values.len() - 1)]
}

/// Measure energy per inference on every available device, under several loads, repeatedly.
///
/// **Repeats are not optional rigour here, they are what makes the headline claim admissible.** A
/// single pass reported an NPU/iGPU difference of a few percent, which is the same order as a
/// laptop package's own drift between two idle samples — so one measurement cannot distinguish
/// "the NPU is slightly cheaper" from "the machine was slightly quieter that minute". The first
/// round is discarded as warm-up and the rest are summarised as median and p95, per the project's
/// own benchmarking rule.
///
/// Idle is re-sampled every round rather than once at each end, so the noise floor reflects drift
/// across the whole run instead of two points of it.
#[must_use]
pub fn measure_power(model_xml: &Path, seconds_per_device: u64, repeats: usize) -> PowerReport {
    use sentin_detect::ov;

    let mut notes = Vec::new();
    let Ok(reader) = energy::Reader::new() else {
        return PowerReport {
            idle_w: 0.0,
            noise_floor_w: 0.0,
            seconds_per_device,
            repeats: 0,
            idle_samples_w: Vec::new(),
            devices: Vec::new(),
            notes: vec![
                "RAPL counters unreadable — energy cannot be measured. On Linux: \
                 sudo chmod a+r /sys/class/powercap/intel-rapl:*/energy_uj"
                    .to_string(),
            ],
        };
    };

    let devices_available = ov::probe(None)
        .map(|r| r.available_devices)
        .unwrap_or_default();

    // Keyed by (device, scenario) so the rounds accumulate into one row each.
    let mut trials: BTreeMap<(String, &str), Vec<PowerTrial>> = BTreeMap::new();
    let mut failures: BTreeMap<String, String> = BTreeMap::new();
    let mut idle_samples = Vec::new();

    for round in 0..=repeats {
        let idle_w = sample_idle(&reader, seconds_per_device.min(10));
        idle_samples.push(idle_w);

        for device in &devices_available {
            if failures.contains_key(device) {
                continue; // a device that refused once will refuse again; do not spend the time
            }
            for (label, target_rps) in POWER_SCENARIOS {
                let start = reader.sample();
                let outcome = ov::run_at_rate(
                    model_xml,
                    device,
                    std::time::Duration::from_secs(seconds_per_device),
                    target_rps,
                );
                let end = reader.sample();
                let elapsed = energy::elapsed(&start, &end);
                let joules = package_joules(&reader, &start, &end);

                match outcome {
                    Ok((count, _)) => {
                        if round == 0 {
                            continue; // warm-up: compilation, caches, and the first clock ramp
                        }
                        let seconds = elapsed.as_secs_f64().max(f64::EPSILON);
                        let package_w = joules / seconds;
                        let active_w = (package_w - idle_w).max(0.0);
                        trials
                            .entry((device.clone(), label))
                            .or_default()
                            .push(PowerTrial {
                                inferences: count,
                                seconds,
                                package_w,
                                active_w,
                                mj_per_inference: if count > 0 {
                                    active_w * seconds * 1000.0 / count as f64
                                } else {
                                    0.0
                                },
                            });
                    }
                    Err(err) => {
                        notes.push(format!("{device} could not run the model: {err}"));
                        failures.insert(device.clone(), err);
                        break;
                    }
                }
            }
        }
    }

    let idle_median = median(&mut idle_samples.clone());
    let noise_floor_w = idle_samples
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max)
        - idle_samples.iter().copied().fold(f64::INFINITY, f64::min);

    let mut devices = Vec::new();
    for ((device, label), rows) in trials {
        let target_rps = POWER_SCENARIOS
            .iter()
            .find(|(name, _)| *name == label)
            .and_then(|(_, rps)| *rps);
        let mut active: Vec<f64> = rows.iter().map(|t| t.active_w).collect();
        let mut mj: Vec<f64> = rows.iter().map(|t| t.mj_per_inference).collect();
        let mut rate: Vec<f64> = rows
            .iter()
            .map(|t| t.inferences as f64 / t.seconds)
            .collect();
        let mut package: Vec<f64> = rows.iter().map(|t| t.package_w).collect();
        let active_median = median(&mut active.clone());
        let spread = active.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - active.iter().copied().fold(f64::INFINITY, f64::min);
        devices.push(DevicePower {
            device,
            scenario: label.to_string(),
            target_rps,
            repeats: rows.len(),
            inferences_per_second: median(&mut rate),
            package_w: median(&mut package),
            idle_w: idle_median,
            active_w: active_median,
            active_w_p95: p95(&mut active),
            mj_per_inference: median(&mut mj.clone()),
            mj_per_inference_p95: p95(&mut mj),
            active_w_spread: spread,
            above_noise: active_median > noise_floor_w,
            trials: rows,
            error: None,
        });
    }
    for (device, err) in failures {
        devices.push(DevicePower {
            device,
            scenario: "n/a".to_string(),
            target_rps: None,
            repeats: 0,
            inferences_per_second: 0.0,
            package_w: 0.0,
            idle_w: idle_median,
            active_w: 0.0,
            active_w_p95: 0.0,
            mj_per_inference: 0.0,
            mj_per_inference_p95: 0.0,
            active_w_spread: 0.0,
            above_noise: false,
            trials: Vec::new(),
            error: Some(err),
        });
    }

    notes.push(format!(
        "Idle sampled once per round: {} values, median {idle_median:.2} W, spread \
         {noise_floor_w:.2} W. That spread is the noise floor — a difference between two devices \
         smaller than it has not been measured.",
        idle_samples.len()
    ));
    notes.push(
        "Package RAPL covers the whole SoC, including the NPU — there is no separate NPU domain. \
         These figures are therefore package power *while* a device was working, and the NPU's own \
         draw is the difference between its row and the CPU row, not an absolute reading."
            .to_string(),
    );
    notes.push(
        "Rows marked `below noise` are upper bounds, not measurements. At low request rates an \
         inference costing tens of millijoules is tens of milliwatts spread over a second, which \
         package RAPL on a laptop cannot separate from the platform's own drift. That is a fact \
         about the instrument, not about the device."
            .to_string(),
    );

    PowerReport {
        idle_w: idle_median,
        noise_floor_w,
        seconds_per_device,
        repeats,
        idle_samples_w: idle_samples,
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
        "  idle {:.2} W (median of {}), noise floor {:.2} W, {} s per run, \
         {} measured repeats + 1 discarded\n",
        report.idle_w,
        report.idle_samples_w.len(),
        report.noise_floor_w,
        report.seconds_per_device,
        report.repeats
    );
    println!(
        "  {:<6}{:<12}{:>9}{:>11}{:>11}{:>9}{:>13}{:>11}",
        "device", "load", "per s", "package W", "active W", "spread", "mJ/infer", "mJ p95"
    );
    for device in &report.devices {
        if let Some(error) = &device.error {
            println!("  {:<6}  refused: {error}", device.device);
            continue;
        }
        // The marker sits on the row rather than in a footnote, because a reader scanning the
        // table is exactly the reader who would otherwise quote an unmeasurable number.
        let flag = if device.above_noise {
            ""
        } else {
            "   <- below noise, upper bound only"
        };
        println!(
            "  {:<6}{:<12}{:>9.1}{:>11.2}{:>11.2}{:>9.2}{:>13.2}{:>11.2}{}",
            device.device,
            device.scenario,
            device.inferences_per_second,
            device.package_w,
            device.active_w,
            device.active_w_spread,
            device.mj_per_inference,
            device.mj_per_inference_p95,
            flag
        );
    }
    for note in &report.notes {
        println!("\n  * {note}");
    }
}
