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
    pub cpu_model: String,
    pub logical_cpus: usize,
    pub os: String,
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
            kernel: read_first_line("/proc/sys/kernel/osrelease").unwrap_or_default(),
            cpu_governor: read_first_line("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
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

        match self.cpu_governor.as_deref() {
            Some("performance") => {}
            Some(other) => warnings.push(format!(
                "cpu governor is '{other}', not 'performance' — frequency scaling will move the \
                 result. Pin it with: sudo cpupower frequency-set -g performance"
            )),
            None => warnings.push(
                "cpu governor unknown (no cpufreq sysfs) — record the power plan manually".into(),
            ),
        }

        if let Some(profile) = &self.platform_profile {
            if profile != "performance" {
                warnings.push(format!(
                    "ACPI platform profile is '{profile}' — the firmware power budget differs \
                     from a 'performance' run"
                ));
            }
        }

        if self.on_ac_power == Some(false) {
            warnings.push(
                "running on battery — power limits and thermal budget differ from AC, and the \
                 result will drift as the battery drains"
                    .into(),
            );
        }

        if self.energy_domains.is_empty() {
            warnings.push("no energy domains available — nothing can be measured".into());
        }

        warnings
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
    fn fingerprint_round_trips_through_json() {
        // Results are merged across machines by reading these back, so the shape must be stable.
        let m = machine(Some("performance"), Some("performance"), Some(true));
        let text = serde_json::to_string(&m).expect("serialises");
        let back: Machine = serde_json::from_str(&text).expect("deserialises");
        assert_eq!(back.cpu_model, m.cpu_model);
        assert_eq!(back.energy_backend, m.energy_backend);
    }
}
