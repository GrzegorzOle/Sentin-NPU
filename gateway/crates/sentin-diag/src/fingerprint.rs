// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Machine fingerprint recorded alongside every measurement.
//!
//! This exists so results from different machines can be put in the same table without lying.
//! A wattage figure means nothing on its own: the same binary on the same silicon draws visibly
//! different power under `powersave` than under `performance`, on battery than on AC, and warm
//! than cold. Two rows in a benchmark table are only comparable if the fields below match — so
//! they travel with the numbers rather than living in someone's memory of how the run was set up.
//!
//! [`Machine::comparability_warnings`] turns the same knowledge into a check the harness can run
//! before it spends a minute measuring something that will not be usable.

use serde::{Deserialize, Serialize};

/// Everything needed to decide whether two measurements may be compared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Machine {
    /// CPU as it names itself. Two measurements from different CPUs describe the CPUs, not the
    /// thing being measured — which is why gateway-overhead results are never merged across them.
    pub cpu_model: String,
    /// Logical CPU count, which sets how much parallelism the CPU inference path can use.
    pub logical_cpus: usize,
    /// Operating system, since the energy backend and its meaning differ by platform.
    pub os: String,
    /// Kernel version. Both the powercap permissions and the NPU driver depend on it.
    pub kernel: String,
    /// CPU frequency governor (Linux). `performance` and `powersave` are not comparable.
    pub cpu_governor: Option<String>,
    /// ACPI platform profile: `performance`, `balanced`, `low-power`.
    pub platform_profile: Option<String>,
    /// True when running on mains. Battery changes both the power limits and the thermal budget.
    pub on_ac_power: Option<bool>,
    /// Energy backend actually used, e.g. `powercap-rapl`. Numbers from different backends are
    /// not interchangeable, however similar the units look.
    pub energy_backend: String,
    /// RAPL (or equivalent) domains found. Records what was measurable, not what was assumed.
    pub energy_domains: Vec<String>,
}

impl Machine {
    /// Detect everything detectable on this host.
    #[must_use]
    pub fn detect(energy_backend: &str, energy_domains: Vec<String>) -> Self {
        Self {
            cpu_model: cpu_model(),
            logical_cpus: std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
            os: os_name(),
            kernel: kernel_version(),
            cpu_governor: cpu_governor(),
            platform_profile: read_first_line("/sys/firmware/acpi/platform_profile"),
            on_ac_power: on_ac_power(),
            energy_backend: energy_backend.to_string(),
            energy_domains,
        }
    }

    /// Conditions that make a measurement hard to compare with another machine's.
    ///
    /// These are warnings, not errors. A run under `powersave` is still a valid measurement of
    /// that configuration — it just must not be put in the same column as a `performance` run,
    /// and the person reading the table six months later will not remember which was which.
    #[must_use]
    pub fn comparability_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        // The advice follows the machine the measurement came from, not the one reading it: a
        // report is routinely opened elsewhere, and telling a Windows operator to run cpupower
        // helps nobody.
        match (self.cpu_governor.as_deref(), self.is_windows()) {
            (Some("performance"), _) => {}
            (Some(plan), true) => {
                if !plan.to_ascii_lowercase().contains("high performance") {
                    warnings.push(format!(
                        "Windows power plan is '{plan}', not 'High performance' - frequency \
                         scaling will move the result. Pin it with: powercfg /setactive SCHEME_MIN"
                    ));
                }
            }
            (Some(other), false) => warnings.push(format!(
                "cpu governor is '{other}', not 'performance' - frequency scaling will move the \
                 result. Pin it with: sudo cpupower frequency-set -g performance"
            )),
            (None, true) => warnings.push(
                "Windows power plan unknown (powercfg did not answer) - record it by hand".into(),
            ),
            (None, false) => warnings.push(
                "cpu governor unknown (no cpufreq sysfs) - record the power plan manually".into(),
            ),
        }

        if let Some(profile) = &self.platform_profile {
            if profile != "performance" {
                warnings.push(format!(
                    "ACPI platform profile is '{profile}' - the firmware power budget differs \
                     from a 'performance' run"
                ));
            }
        }

        if self.on_ac_power == Some(false) {
            warnings.push(
                "running on battery - power limits and thermal budget differ from AC, and the \
                 result will drift as the battery drains"
                    .into(),
            );
        }

        if self.energy_domains.is_empty() {
            if self.is_windows() {
                // Stated rather than left as a blank field: a reader who sees nothing cannot tell
                // an unmeasurable platform from a failed measurement.
                warnings.push(
                    "Windows exposes no RAPL, so energy cannot be measured here at all. Intel PCM \
                     or an HWiNFO CSV log is the substitute and is not interchangeable with RAPL; \
                     energy work belongs on a Linux installation."
                        .into(),
                );
            } else {
                warnings.push("no energy domains available - nothing can be measured".into());
            }
        }

        warnings
    }

    /// Whether this fingerprint was taken on Windows, which changes what is knowable.
    #[must_use]
    pub fn is_windows(&self) -> bool {
        self.os.to_ascii_lowercase().contains("windows")
    }

    /// A one-line label for a results table.
    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "{} / {} / {} / governor={} / profile={}",
            self.cpu_model,
            self.os,
            self.energy_backend,
            self.cpu_governor.as_deref().unwrap_or("?"),
            self.platform_profile.as_deref().unwrap_or("?"),
        )
    }
}

/// The CPU frequency policy under whichever name the platform gives it.
///
/// Linux has a cpufreq governor; Windows has a power plan, which is the same knob for the purpose
/// of a benchmark. Both land in one field so a results table has one column rather than two, and
/// [`Machine::comparability_warnings`] reads the platform back off `os` before advising a fix.
fn cpu_governor() -> Option<String> {
    if cfg!(windows) {
        return windows_power_plan();
    }
    read_first_line("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
}

/// Parse the active scheme out of `powercfg /getactivescheme`.
///
/// The name is localised - a Polish Windows says "Zrownowazony" - so it is recorded verbatim
/// rather than matched against an English word. What matters for comparability is that two runs
/// carry the *same* string, not which string it is.
fn windows_power_plan() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let output = std::process::Command::new("powercfg")
        .arg("/getactivescheme")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let start = text.find('(')?;
    let end = text[start..].find(')')?;
    let name = text[start + 1..start + end].trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Kernel or OS build version, whichever the platform exposes.
fn kernel_version() -> String {
    if let Some(release) = read_first_line("/proc/sys/kernel/osrelease") {
        return release;
    }
    if cfg!(windows) {
        // No /proc here. The build is what a Windows reader would quote, and it is what the
        // bundle's run.ps1 records beside this, so the two agree.
        if let Ok(output) = std::process::Command::new("cmd").args(["/c", "ver"]).output() {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !text.is_empty() {
                return text;
            }
        }
    }
    String::new()
}

fn cpu_model() -> String {
    // Linux exposes this in /proc/cpuinfo; elsewhere fall back to the target triple, which at
    // least records the architecture rather than inventing a model name.
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("model name"))
                .and_then(|line| line.split_once(':'))
                .map(|(_, value)| value.trim().to_string())
        })
        .or_else(|| {
            // Windows has no /proc/cpuinfo. PROCESSOR_IDENTIFIER is not the marketing name, but
            // "Intel64 Family 6 Model 183" identifies the part, and inventing nothing beats
            // recording the architecture alone.
            std::env::var("PROCESSOR_IDENTIFIER")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| std::env::consts::ARCH.to_string())
}

fn os_name() -> String {
    read_first_line("/etc/os-release")
        .filter(|line| line.starts_with("PRETTY_NAME="))
        .map(|line| {
            line.trim_start_matches("PRETTY_NAME=")
                .trim_matches('"')
                .to_string()
        })
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

/// Whether any mains supply reports itself online.
fn on_ac_power() -> Option<bool> {
    let entries = std::fs::read_dir("/sys/class/power_supply").ok()?;
    let mut saw_mains = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if read_first_line(path.join("type").to_str()?).as_deref() != Some("Mains") {
            continue;
        }
        saw_mains = true;
        if read_first_line(path.join("online").to_str()?).as_deref() == Some("1") {
            return Some(true);
        }
    }
    saw_mains.then_some(false)
}

fn read_first_line(path: impl AsRef<std::path::Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.lines().next().map(str::to_string))
        .filter(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(governor: Option<&str>, profile: Option<&str>, ac: Option<bool>) -> Machine {
        Machine {
            cpu_model: "Test CPU".into(),
            logical_cpus: 8,
            os: "Test OS".into(),
            kernel: "0.0".into(),
            cpu_governor: governor.map(str::to_string),
            platform_profile: profile.map(str::to_string),
            on_ac_power: ac,
            energy_backend: "powercap-rapl".into(),
            energy_domains: vec!["package-0".into()],
        }
    }

    #[test]
    fn a_pinned_performance_machine_on_ac_has_no_warnings() {
        let m = machine(Some("performance"), Some("performance"), Some(true));
        assert!(
            m.comparability_warnings().is_empty(),
            "{:?}",
            m.comparability_warnings()
        );
    }

    #[test]
    fn powersave_governor_is_flagged_with_the_fix() {
        let warnings =
            machine(Some("powersave"), Some("performance"), Some(true)).comparability_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("powersave"));
        assert!(
            warnings[0].contains("cpupower"),
            "the warning must say how to fix it"
        );
    }

    #[test]
    fn battery_and_balanced_profile_are_both_flagged() {
        let warnings =
            machine(Some("performance"), Some("balanced"), Some(false)).comparability_warnings();
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|w| w.contains("platform profile")));
        assert!(warnings.iter().any(|w| w.contains("battery")));
    }

    #[test]
    fn a_machine_with_no_energy_domains_is_flagged_as_unmeasurable() {
        let mut m = machine(Some("performance"), Some("performance"), Some(true));
        m.energy_domains.clear();
        assert!(m
            .comparability_warnings()
            .iter()
            .any(|w| w.contains("nothing can be measured")));
    }

    #[test]
    fn detection_on_this_host_produces_a_usable_label() {
        // Must not panic wherever CI runs it, including containers with no cpufreq or power supply.
        let m = Machine::detect("powercap-rapl", vec!["package-0".into()]);
        assert!(!m.label().is_empty());
        assert!(m.logical_cpus > 0);
    }

    #[test]
    fn a_windows_report_gets_windows_advice_not_cpupower() {
        // Reports are read on other machines, so the advice follows the fingerprint's own os.
        let mut m = machine(Some("Balanced"), None, Some(true));
        m.os = "windows".into();
        let warnings = m.comparability_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("powercfg")),
            "{warnings:?}"
        );
        assert!(
            !warnings.iter().any(|w| w.contains("cpupower")),
            "Linux advice must not appear on a Windows report: {warnings:?}"
        );
    }

    #[test]
    fn windows_says_energy_is_unmeasurable_rather_than_leaving_a_blank() {
        let mut m = machine(Some("High performance"), None, Some(true));
        m.os = "windows".into();
        m.energy_domains.clear();
        let warnings = m.comparability_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("no RAPL")),
            "the reason must be stated, not implied by an empty list: {warnings:?}"
        );
    }

    #[test]
    fn fingerprint_round_trips_through_json() {
        // Results are merged across machines by reading these back, so the shape must be stable.
        let m = machine(Some("performance"), Some("performance"), Some(true));
        let text = serde_json::to_string(&m).expect("serialises");
        let back: Machine = serde_json::from_str(&text).expect("deserialises");
        assert_eq!(back.cpu_model, m.cpu_model);
        assert_eq!(back.energy_backend, m.energy_backend);
    }
}
