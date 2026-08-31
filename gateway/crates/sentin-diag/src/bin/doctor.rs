// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! `sentin-doctor` — the standalone diagnostic, built to run on machines with no toolchain.
//!
//! One command, one JSON file. That is the whole design goal: access to Intel NPU hardware is
//! scarce and often remote, so the session should be "copy this over, run it, send the file back"
//! rather than an attempt to reproduce a development environment over SSH.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return ExitCode::SUCCESS;
    }

    let flag = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());

    let model = flag("--model").map(PathBuf::from);
    let report = sentin_diag::doctor::run(model.as_deref());
    sentin_diag::doctor::print(&report);

    // Power is opt-in: it takes minutes and needs readable RAPL counters, so it must not be a
    // surprise cost on a machine somebody lent us for half an hour.
    if args.iter().any(|a| a == "--power") {
        let seconds = flag("--power-seconds")
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);
        // Five measured repeats is the project's own benchmarking rule, not a preference: the
        // device differences this metric reports are the same size as the platform's drift, so a
        // single pass cannot tell them apart.
        let repeats = flag("--power-repeats")
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);
        match model.as_deref() {
            Some(path) => {
                let power = sentin_diag::doctor::measure_power(path, seconds, repeats);
                sentin_diag::doctor::print_power(&power);
                if let Some(path) = flag("--power-json") {
                    match serde_json::to_string_pretty(&power) {
                        Ok(text) => match std::fs::write(&path, text) {
                            Ok(()) => println!("\nwrote {path}"),
                            Err(err) => eprintln!("could not write {path}: {err}"),
                        },
                        Err(err) => eprintln!("could not serialise power report: {err}"),
                    }
                }
            }
            None => eprintln!("\n--power needs --model <openvino_model.xml>"),
        }
    }

    if let Some(path) = flag("--json") {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => match std::fs::write(&path, text) {
                Ok(()) => println!("\nwrote {path}"),
                Err(err) => {
                    eprintln!("could not write {path}: {err}");
                    return ExitCode::FAILURE;
                }
            },
            Err(err) => {
                eprintln!("could not serialise report: {err}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn print_help() {
    println!(
        "sentin-doctor - report what this machine can do with OpenVINO\n\n\
         USAGE:\n  \
           sentin-doctor [--model <openvino_model.xml>] [--json <file>] [--power]\n\n\
         OPTIONS:\n  \
           --model <xml>        compile and run this IR on every device (strongly recommended:\n                       \
                                enumeration alone proves nothing)\n  \
           --json <file>        write the machine-readable report, for an npu-report issue\n  \
           --power              also measure energy per device (needs readable RAPL counters)\n  \
           --power-seconds <n>  seconds per measurement (default 20)\n  \
           --power-repeats <n>  measured repeats per device and load, after one discarded\n                       \
                                warm-up round (default 5). Fewer than 5 cannot separate a\n                       \
                                device difference from the platform's own drift.\n  \
           --power-json <file>  write the energy report, including every individual repeat\n\n\
         If the OpenVINO libraries cannot be found, they need *unversioned* symlinks on\n\
         LD_LIBRARY_PATH - the bundled run.sh does that for you."
    );
}
