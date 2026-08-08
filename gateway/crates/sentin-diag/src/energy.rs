// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Package energy measurement via the Linux powercap RAPL interface.
//!
//! This is what turns "the gateway adds 0.07 ms" into "the gateway costs N mJ per request", which
//! is the number that belongs in a report: latency says whether users notice, energy says what it
//! costs to run, and on a battery-powered AI PC the second one is the question people actually ask.
//!
//! # What RAPL can and cannot tell you on an Intel Core Ultra
//!
//! RAPL reports **package** energy. The NPU sits inside that package and, on the machines checked
//! so far, does **not** get its own powercap domain. So:
//!
//! * you can measure what the whole SoC drew while a workload ran;
//! * you **cannot** read "NPU watts" directly;
//! * NPU energy is obtained by *differencing* — run the same workload with `device=NPU` and
//!   `device=CPU`, subtract, and attribute the difference.
//!
//! Anyone quoting an NPU power figure taken straight from a RAPL domain is quoting the whole
//! package. Enumerate the domains on the target machine first ([`domains`]) and record what was
//! actually there, rather than assuming this holds for every generation.
//!
//! # Permissions
//!
//! Since the PLATYPUS side-channel disclosure, `energy_uj` is root-readable only. [`Reader::new`]
//! reports that as [`EnergyError::PermissionDenied`] together with the exact remedy, rather than
//! silently reporting zero — a measurement harness that quietly produces nothing is worse than one
//! that refuses to start.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const POWERCAP: &str = "/sys/class/powercap";

/// Name of the energy backend, recorded with every measurement.
///
/// Numbers from different backends are **not** interchangeable — RAPL counts package energy in
/// silicon, Intel PCM reads the same counters through a kernel driver with its own sampling, and a
/// battery gauge measures the whole platform including the screen. Putting them in one column
/// would be a category error, so the name travels with the result.
pub const BACKEND: &str = if cfg!(target_os = "linux") {
    "powercap-rapl"
} else {
    "unavailable"
};

/// Why energy cannot be measured here.
///
/// Each variant refuses rather than returning zeros: a wattage of 0.0 looks like a result and
/// would be quoted as one.
#[derive(Debug, thiserror::Error)]
pub enum EnergyError {
    /// The kernel exposes no powercap domains — an old kernel, a VM, or a CPU without RAPL.
    #[error("no RAPL domains found under {POWERCAP} — this kernel or CPU does not expose them")]
    Unsupported,
    #[error(
        "no energy backend on this platform.\n\
         Windows has no powercap sysfs; RAPL is reachable only through a signed kernel driver.\n\
         Options, in order of preference for this project:\n  \
           1. Run the energy measurements on the Intel Core Ultra *Linux* installation — same\n     \
              powercap interface as the dev machine, so the numbers are directly comparable.\n  \
           2. Intel PCM (`pcm-power.exe`, needs its driver and an elevated shell), recorded as a\n     \
              separate backend and never mixed into a RAPL column.\n  \
           3. HWiNFO CSV logging, for indicative figures only.\n\
         Windows remains the platform for functional verification (Phase 5)."
    )]
    /// Not Linux. The message lists the alternatives in the order this project prefers them.
    UnsupportedPlatform,
    /// The counters exist but are root-only since the PLATYPUS mitigation. The message carries
    /// both the one-off and the persistent fix.
    #[error(
        "RAPL counters are root-readable only (PLATYPUS mitigation).\n\
         Grant read access once with:\n  \
           sudo chmod a+r /sys/class/powercap/intel-rapl:*/energy_uj\n\
         or persistently, as root:\n  \
           echo 'SUBSYSTEM==\"powercap\", ACTION==\"add\", RUN+=\"/bin/chmod a+r /sys%p/energy_uj\"' \\\n    \
             > /etc/udev/rules.d/99-rapl-readable.rules\n\
         Alternatively run the harness itself under sudo."
    )]
    PermissionDenied,
}

/// One RAPL domain, e.g. `package-0` or `core`.
#[derive(Debug, Clone)]
pub struct Domain {
    /// Domain name as the kernel reports it, e.g. `package-0`, `core`, `uncore`. Which domains
    /// exist differs between machines and must be recorded, never assumed.
    pub name: String,
    path: PathBuf,
    /// Counter wrap point. RAPL counters are cumulative and wrap; deltas must account for it.
    max_range_uj: u64,
}

impl Domain {
    fn read_uj(&self) -> std::io::Result<u64> {
        let raw = std::fs::read_to_string(&self.path)?;
        raw.trim()
            .parse::<u64>()
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
    }
}

/// Enumerate the RAPL domains this machine exposes.
///
/// Worth recording verbatim in any report: which domains exist is generation-specific, and it is
/// the difference between "package energy" and a claim about a specific block of silicon.
#[must_use]
pub fn domains() -> Vec<Domain> {
    let Ok(entries) = std::fs::read_dir(POWERCAP) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let energy = dir.join("energy_uj");
        if !energy.exists() {
            continue;
        }
        let name = read_trimmed(&dir.join("name")).unwrap_or_else(|| {
            dir.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let max_range_uj = read_trimmed(&dir.join("max_energy_range_uj"))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(u64::MAX);
        found.push(Domain {
            name,
            path: energy,
            max_range_uj,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// A snapshot of every domain's counter at one instant.
#[derive(Debug, Clone)]
pub struct Sample {
    readings: Vec<(String, u64)>,
    at: Instant,
}

/// Reads energy counters, handling counter wraparound between samples.
#[derive(Debug)]
pub struct Reader {
    domains: Vec<Domain>,
}

impl Reader {
    /// Open the RAPL interface.
    ///
    /// # Errors
    /// [`EnergyError::Unsupported`] when no domains exist, [`EnergyError::PermissionDenied`] when
    /// they exist but cannot be read.
    pub fn new() -> Result<Self, EnergyError> {
        if !cfg!(target_os = "linux") {
            return Err(EnergyError::UnsupportedPlatform);
        }
        let domains = domains();
        if domains.is_empty() {
            return Err(EnergyError::Unsupported);
        }
        if domains.iter().all(|d| d.read_uj().is_err()) {
            return Err(EnergyError::PermissionDenied);
        }
        Ok(Self { domains })
    }

    /// Names of every readable domain, for recording alongside the numbers.
    #[must_use]
    pub fn domain_names(&self) -> Vec<String> {
        self.domains.iter().map(|d| d.name.clone()).collect()
    }

    /// Take a snapshot of all readable domains.
    #[must_use]
    pub fn sample(&self) -> Sample {
        let readings = self
            .domains
            .iter()
            .filter_map(|d| d.read_uj().ok().map(|uj| (d.name.clone(), uj)))
            .collect();
        Sample {
            readings,
            at: Instant::now(),
        }
    }

    /// Energy consumed per domain between two samples, in microjoules.
    ///
    /// Counters are cumulative and wrap at `max_energy_range_uj`; a naive subtraction would report
    /// a huge negative (as an underflowed unsigned) exactly once per wrap and poison the run.
    #[must_use]
    pub fn delta_uj(&self, start: &Sample, end: &Sample) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for (name, end_uj) in &end.readings {
            let Some((_, start_uj)) = start.readings.iter().find(|(n, _)| n == name) else {
                continue;
            };
            let wrap = self
                .domains
                .iter()
                .find(|d| &d.name == name)
                .map_or(u64::MAX, |d| d.max_range_uj);
            let delta = if end_uj >= start_uj {
                end_uj - start_uj
            } else {
                wrap.saturating_sub(*start_uj).saturating_add(*end_uj)
            };
            out.push((name.clone(), delta));
        }
        out
    }
}

/// Elapsed wall-clock time between two samples.
#[must_use]
pub fn elapsed(start: &Sample, end: &Sample) -> Duration {
    end.at.saturating_duration_since(start.at)
}

/// Energy accounting for one measured interval.
#[derive(Debug, Clone)]
pub struct Measurement {
    /// Which RAPL domain this accounts for.
    pub domain: String,
    /// Energy consumed over the interval, wrap-around already handled.
    pub energy_j: f64,
    /// How long the interval lasted.
    pub duration: Duration,
}

impl Measurement {
    /// Mean power over the interval.
    #[must_use]
    pub fn watts(&self) -> f64 {
        let seconds = self.duration.as_secs_f64();
        if seconds <= 0.0 {
            0.0
        } else {
            self.energy_j / seconds
        }
    }

    /// Energy attributable to work, with an idle baseline of `idle_w` removed.
    ///
    /// Subtracting idle is not optional. A laptop package draws several watts doing nothing, which
    /// on a short run dwarfs the workload and would make the gateway look far more expensive than
    /// it is. Clamped at zero: a negative result means measurement noise exceeded the signal, and
    /// reporting negative energy would be worse than reporting none.
    #[must_use]
    pub fn active_energy_j(&self, idle_w: f64) -> f64 {
        (self.energy_j - idle_w * self.duration.as_secs_f64()).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_of(pairs: &[(&str, u64)], at: Instant) -> Sample {
        Sample {
            readings: pairs.iter().map(|(n, v)| ((*n).to_string(), *v)).collect(),
            at,
        }
    }

    fn reader_with(name: &str, max_range_uj: u64) -> Reader {
        Reader {
            domains: vec![Domain {
                name: name.to_string(),
                path: PathBuf::from("/nonexistent"),
                max_range_uj,
            }],
        }
    }

    #[test]
    fn plain_delta_is_a_subtraction() {
        let reader = reader_with("package-0", 1_000_000);
        let now = Instant::now();
        let delta = reader.delta_uj(
            &sample_of(&[("package-0", 100)], now),
            &sample_of(&[("package-0", 450)], now),
        );
        assert_eq!(delta, vec![("package-0".to_string(), 350)]);
    }

    #[test]
    fn wraparound_does_not_produce_a_nonsense_delta() {
        // The counter wraps at max_energy_range_uj. Without handling it, a run that happens to
        // straddle the wrap reports an absurd figure and silently ruins the whole measurement.
        let reader = reader_with("package-0", 1_000);
        let now = Instant::now();
        let delta = reader.delta_uj(
            &sample_of(&[("package-0", 990)], now),
            &sample_of(&[("package-0", 10)], now),
        );
        assert_eq!(
            delta,
            vec![("package-0".to_string(), 20)],
            "10 before + 10 after"
        );
    }

    #[test]
    fn idle_subtraction_isolates_the_workload() {
        let m = Measurement {
            domain: "package-0".into(),
            energy_j: 100.0,
            duration: Duration::from_secs(10),
        };
        assert!((m.watts() - 10.0).abs() < 1e-9);
        // 4 W of idle over 10 s is 40 J of the 100 J measured.
        assert!((m.active_energy_j(4.0) - 60.0).abs() < 1e-9);
    }

    #[test]
    fn noise_below_the_idle_baseline_reports_zero_not_negative() {
        let m = Measurement {
            domain: "package-0".into(),
            energy_j: 30.0,
            duration: Duration::from_secs(10),
        };
        assert_eq!(m.active_energy_j(4.0), 0.0);
    }

    #[test]
    fn domain_discovery_never_panics_on_this_machine() {
        // Whether or not RAPL is readable here, enumeration must be safe to call.
        let _ = domains();
    }
}
