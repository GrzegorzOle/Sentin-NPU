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

use crate::fingerprint::Machine;

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub sentin_version: String,
    pub machine: Machine,
    pub accelerator_drivers: Vec<DriverInfo>,
    pub openvino: Option<ov::Report>,
    pub openvino_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DriverInfo {
    pub name: String,
    pub version: Option<String>,
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
