// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! The Sentin-NPU desktop console.
//!
//! Started from the Start Menu it opens a window. Given `--report` it writes the HTML summary and
//! exits, which is what a scheduled task wants and what the tests exercise.

// A window, not a console: this tool exists for somebody who should not have to look at one, and a
// black rectangle appearing beside the window reads as a fault. The cost is that `--report` output
// is not shown when it is run from a terminal on Windows unless it is redirected - the report file
// and the exit code carry the result, which is what a scheduled task reads anyway.
#![cfg_attr(windows, windows_subsystem = "windows")]
#![warn(missing_docs)]

use std::path::PathBuf;
use std::process::ExitCode;

use sentin_ui::{app::App, report::Report, text::Lang};

/// Where an installed gateway keeps its configuration.
fn default_config_path() -> PathBuf {
    #[cfg(windows)]
    {
        let root = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".to_string());
        PathBuf::from(root).join("Sentin-NPU").join("config.yaml")
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".config")
            .join("sentin-npu")
            .join("config.yaml")
    }
}

const USAGE: &str = "\
sentin-ui - Sentin-NPU console

  sentin-ui [CONFIG]              open the window (default: the installed configuration)
  sentin-ui --report AUDIT [OUT]  write an HTML report from an audit trail and exit
  sentin-ui --lang en|pl          force the interface language
  sentin-ui --help

With no arguments it edits the installed configuration and reports on the audit trail that
configuration points at.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut lang = Lang::detect();
    let mut rest = Vec::new();

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--lang" => match iter.next().as_deref() {
                Some("pl") => lang = Lang::Pl,
                Some("en") => lang = Lang::En,
                other => {
                    eprintln!("--lang takes `en` or `pl`, not {other:?}");
                    return ExitCode::FAILURE;
                }
            },
            _ => rest.push(arg),
        }
    }

    if rest.first().is_some_and(|a| a == "--report") {
        return match write_report(&rest[1..], lang) {
            Ok(path) => {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }

    let config = rest.first().map_or_else(default_config_path, PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([860.0, 720.0])
            .with_min_inner_size([680.0, 520.0]),
        ..Default::default()
    };

    match eframe::run_native(
        lang.window_title(),
        options,
        Box::new(move |_cc| Ok(Box::new(App::new(config, lang)))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Write a report without opening a window.
fn write_report(args: &[String], lang: Lang) -> Result<PathBuf, String> {
    let source = args
        .first()
        .ok_or_else(|| "--report needs the path of an audit trail".to_string())?;
    let source = PathBuf::from(source);
    let out = args.get(1).map_or_else(
        || source.with_file_name("sentin-report.html"),
        PathBuf::from,
    );

    let text =
        std::fs::read_to_string(&source).map_err(|e| format!("{}: {e}", source.display()))?;
    let report = Report::from_jsonl(&text);
    if report.is_empty() {
        return Err(format!("{}: no events to report on", source.display()));
    }
    let html = report.to_html(lang, &source.to_string_lossy());
    std::fs::write(&out, html).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(out)
}
