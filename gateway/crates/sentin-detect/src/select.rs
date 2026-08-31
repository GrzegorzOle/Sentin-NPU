// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Choosing the inference device by measurement rather than by assumption.
//!
//! The original `AUTO` was a fixed cascade, NPU before GPU before CPU, and it is wrong the moment
//! `GPU` does not mean what it means on an Intel laptop. Measured on a Windows workstation on
//! 2026-08-31: OpenVINO enumerated `["CPU", "GPU"]`, the `GPU` was a discrete RTX 3060 reached
//! through the OpenCL ICD, and it ran the NER model at **224.8 ms** against the CPU's **8.7 ms**.
//! The cascade picked it, so `AUTO` selected a device 26x slower than the one beside it and blew a
//! 150 ms budget with a 228 ms pipeline. Enumeration had proven the device existed; nothing had
//! asked whether it was any good.
//!
//! So every device here is timed on the real IR before it is considered, and anything slower than
//! the configured ceiling is rejected outright with its measurement attached. Only among the
//! survivors does preference apply, and which preference is a policy:
//!
//! - [`Objective::Cost`] (the default) prefers the cheaper accelerator - NPU, then an integrated
//!   GPU, then the CPU, and a discrete GPU last. This is not a guess: on a Core Ultra 7 258V at a
//!   realistic 10 rps the NPU costs 78.21 mJ per inference against the iGPU's 160.08 and the CPU's
//!   724.14, so at equal latency the NPU is the right answer by a factor of two. A discrete card
//!   ranks below the CPU because spending a 170 W device on a 9 ms inference is a poor trade even
//!   when it wins on time.
//! - [`Objective::Latency`] simply takes the fastest measurement.
//!
//! Both are gated by the same measured ceiling, so neither can select the RTX 3060 case above. The
//! difference only shows up among devices that have already proven they can do the work in time.
//!
//! Measuring costs a compile per device - on that Windows box 0.8 s for the CPU and 4.5 s for the
//! GPU - which is too much to pay at every start, so the result is cached against the model, the
//! runtime version and the device list. Change any of those and it is measured again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use openvino::{Core, DeviceType, PropertyKey};
use serde::{Deserialize, Serialize};

/// What "best" means once every candidate has proven it meets the latency ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Objective {
    /// Prefer the device that costs least to run. Ties on class are broken by measured latency.
    Cost,
    /// Prefer the fastest measured device, whatever it costs to run.
    Latency,
}

impl Objective {
    /// Parse a configuration value; anything unrecognised is `None` so the caller can complain.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "cost" | "energy" => Some(Self::Cost),
            "latency" | "speed" => Some(Self::Latency),
            _ => None,
        }
    }
}

/// How `AUTO` should choose, as an operator configures it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    /// Whether to time the devices at all. `false` restores the old fixed cascade, which exists
    /// for a machine where a five-second probe at start-up is worse than a wrong first choice.
    pub measure: bool,
    /// What to optimise for among devices that meet the ceiling.
    pub objective: Objective,
    /// Measured steady-state inference a device must beat, in milliseconds.
    pub ceiling_ms: f64,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            measure: true,
            objective: Objective::Cost,
            // The NPU budget from M2b: a device that cannot hold it is not behaving like an
            // accelerator, whatever it calls itself.
            ceiling_ms: 80.0,
        }
    }
}

/// The kind of silicon behind an OpenVINO device name, which is what running cost tracks.
///
/// `GPU` alone cannot answer this: on a Core Ultra it is an integrated Arc sharing the package
/// power budget, and on a desktop it can be a discrete card drawing more than the rest of the
/// machine together. The plugin's own `DEVICE_TYPE` property separates them, and the full device
/// name is the fallback when it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Class {
    /// A neural accelerator. Cheapest per inference wherever this project has measured one.
    Npu,
    /// A GPU sharing the CPU package - Intel Arc iGPU and similar.
    IntegratedGpu,
    /// A general-purpose CPU.
    Cpu,
    /// A discrete graphics card on its own power budget.
    DiscreteGpu,
}

impl Class {
    /// Running-cost order, lowest first. See the module docs for the measurements behind it.
    #[must_use]
    pub fn cost_rank(self) -> u8 {
        match self {
            Self::Npu => 0,
            Self::IntegratedGpu => 1,
            Self::Cpu => 2,
            Self::DiscreteGpu => 3,
        }
    }
}

/// Classify a device from its OpenVINO name and whatever the plugin says about itself.
///
/// `device_type` is the plugin's `DEVICE_TYPE` (`integrated` / `discrete`) and wins when present.
/// Without it the full name decides: a name mentioning a non-Intel vendor is treated as discrete,
/// because an integrated GPU that OpenVINO can reach is an Intel one in every configuration this
/// project has seen.
#[must_use]
pub fn classify(device: &str, full_name: Option<&str>, device_type: Option<&str>) -> Class {
    if device.starts_with("NPU") {
        return Class::Npu;
    }
    if device.starts_with("CPU") {
        return Class::Cpu;
    }
    if !device.starts_with("GPU") {
        // Unknown accelerators are ranked as a CPU: no claim either way, and no free promotion
        // above a device whose cost this project has actually measured.
        return Class::Cpu;
    }

    if let Some(kind) = device_type {
        let kind = kind.to_ascii_lowercase();
        if kind.contains("integrated") {
            return Class::IntegratedGpu;
        }
        if kind.contains("discrete") {
            return Class::DiscreteGpu;
        }
    }

    let name = full_name.unwrap_or_default().to_ascii_lowercase();
    if name.contains("nvidia") || name.contains("radeon rx") || name.contains("dgpu") {
        return Class::DiscreteGpu;
    }
    if name.contains("intel") {
        return Class::IntegratedGpu;
    }
    Class::DiscreteGpu
}

/// One device, measured on the real model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trial {
    /// OpenVINO's device name, as passed to `compile_model`.
    pub device: String,
    /// The device's description of itself, which is what tells a reader what `GPU` meant here.
    pub full_name: Option<String>,
    /// Running-cost class derived from the two fields above.
    pub class: Class,
    /// Time to compile the graph. Cold: the caller is expected to say so when quoting it.
    pub compile_ms: Option<f64>,
    /// Median inference once warm. This is the number the ceiling is applied to.
    pub steady_ms: Option<f64>,
    /// What the device said when it refused. A refusal is a measurement too.
    pub error: Option<String>,
}

impl Trial {
    /// Whether this device compiled the model and produced a timing.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.error.is_none() && self.steady_ms.is_some()
    }
}

/// Why a measured device was not selected, in the operator's words rather than the code's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rejection {
    /// The device that was set aside.
    pub device: String,
    /// The reason, already formatted for a log line.
    pub reason: String,
}

/// The outcome of a selection: what was chosen, what else was considered, and why.
///
/// Everything needed to explain the decision is here rather than in a log call, because the same
/// explanation has to reach a startup log, a `--doctor` report and a test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    /// The device to use. `None` when nothing survived, which is a real outcome on a machine whose
    /// only accelerator refuses the model.
    pub device: Option<String>,
    /// Survivors in preference order, best first.
    pub ranked: Vec<Trial>,
    /// Devices that were measured and set aside, each with its reason.
    pub rejected: Vec<Rejection>,
    /// The objective that ordered the survivors.
    pub objective: Objective,
    /// The ceiling that was applied, in milliseconds.
    pub ceiling_ms: f64,
    /// Set when no device met the ceiling and the fastest one was taken anyway.
    ///
    /// The ceiling exists to stop `AUTO` preferring a device that merely exists, not to switch
    /// layer 2 off on a machine where everything is slow. A gateway inspecting slowly is worth more
    /// than a gateway not inspecting, so the ceiling rejects *relative* to a working alternative
    /// and gives up its veto when there is none. The caller is expected to log this loudly.
    pub over_ceiling: bool,
    /// Whether the measurements came from cache rather than from a fresh probe.
    pub from_cache: bool,
}

impl Choice {
    /// The devices to try, best first - the choice followed by the remaining survivors.
    ///
    /// A device that compiles during selection can still fail later, so the caller keeps the rest
    /// as escape routes rather than betting everything on the winner.
    #[must_use]
    pub fn candidates(&self) -> Vec<String> {
        self.ranked.iter().map(|t| t.device.clone()).collect()
    }

    /// A one-line summary for a startup log: the choice, and what it beat.
    #[must_use]
    pub fn summary(&self) -> String {
        let chosen = self.device.as_deref().unwrap_or("none");
        let measured = self
            .ranked
            .iter()
            .chain(
                // Rejected devices carry no timing here, so only the survivors are quoted; the
                // rejections are logged separately with their reasons.
                std::iter::empty(),
            )
            .map(|t| match t.steady_ms {
                Some(ms) => format!("{}={ms:.1}ms", t.device),
                None => format!("{}=?", t.device),
            })
            .collect::<Vec<_>>()
            .join(" ");
        let source = if self.from_cache { "cached" } else { "measured" };
        format!(
            "device={chosen} objective={:?} ceiling={:.0}ms {source} [{measured}]",
            self.objective, self.ceiling_ms
        )
    }
}

/// Measure every device on the real model, then rank the ones that meet `ceiling_ms`.
///
/// This is the whole mechanism: nothing is preferred before it has been timed, and the preference
/// among timed devices is [`Objective`].
#[must_use]
pub fn rank(trials: Vec<Trial>, objective: Objective, ceiling_ms: f64) -> Choice {
    let mut ranked = Vec::new();
    let mut too_slow = Vec::new();
    let mut rejected = Vec::new();

    for trial in trials {
        match (&trial.error, trial.steady_ms) {
            (Some(err), _) => rejected.push(Rejection {
                device: trial.device.clone(),
                reason: format!("refused the model: {err}"),
            }),
            (None, None) => rejected.push(Rejection {
                device: trial.device.clone(),
                reason: "not measured".to_string(),
            }),
            (None, Some(ms)) if ms > ceiling_ms => too_slow.push((trial, ms)),
            (None, Some(_)) => ranked.push(trial),
        }
    }

    // The ceiling is a comparison, not a switch. With something inside it, everything outside is
    // rejected on the spot; with nothing inside it, the fastest slow device still beats no
    // inspection at all, and the caller is told what it settled for.
    let over_ceiling = ranked.is_empty() && !too_slow.is_empty();
    if over_ceiling {
        too_slow.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let ceiling = ceiling_ms;
        ranked.extend(too_slow.into_iter().map(|(trial, ms)| {
            rejected.push(Rejection {
                device: trial.device.clone(),
                reason: format!(
                    "{ms:.1} ms per inference is over the {ceiling:.0} ms ceiling, and nothing on \
                     this machine is under it"
                ),
            });
            trial
        }));
    } else {
        rejected.extend(too_slow.into_iter().map(|(trial, ms)| Rejection {
            device: trial.device,
            reason: format!("{ms:.1} ms per inference is over the {ceiling_ms:.0} ms ceiling"),
        }));
    }

    // Sort is stable, so the secondary key genuinely breaks ties in the primary one.
    ranked.sort_by(|a, b| {
        let (a_ms, b_ms) = (a.steady_ms.unwrap_or(f64::MAX), b.steady_ms.unwrap_or(f64::MAX));
        let by_time = a_ms.partial_cmp(&b_ms).unwrap_or(std::cmp::Ordering::Equal);
        match objective {
            Objective::Latency => by_time,
            Objective::Cost => a
                .class
                .cost_rank()
                .cmp(&b.class.cost_rank())
                .then(by_time),
        }
    });

    Choice {
        device: ranked.first().map(|t| t.device.clone()),
        ranked,
        rejected,
        objective,
        ceiling_ms,
        over_ceiling,
        from_cache: false,
    }
}

/// Time every enumerated device on `model_xml`.
///
/// A device that refuses is recorded with its refusal rather than dropped: the caller needs to be
/// able to say *why* only the CPU was left.
///
/// # Errors
/// Returns the OpenVINO message when the model itself cannot be read, which is not a per-device
/// condition and would otherwise be reported once per device.
pub fn measure_all(
    core: &mut Core,
    model_xml: &Path,
    available: &[String],
) -> Result<Vec<Trial>, String> {
    let mut trials = Vec::with_capacity(available.len());
    for name in available {
        let device = DeviceType::from(name.as_str());
        let full_name = core.get_property(&device, &PropertyKey::DeviceFullName).ok();
        let device_type = core
            .get_property(&device, &PropertyKey::Other("DEVICE_TYPE".into()))
            .ok();
        let class = classify(name, full_name.as_deref(), device_type.as_deref());

        let mut trial = Trial {
            device: name.clone(),
            full_name,
            class,
            compile_ms: None,
            steady_ms: None,
            error: None,
        };
        match crate::ov::measure_device(core, name, model_xml) {
            Ok((compile_ms, _first_ms, steady_ms)) => {
                trial.compile_ms = Some(compile_ms);
                trial.steady_ms = Some(steady_ms);
            }
            Err(err) => trial.error = Some(err),
        }
        trials.push(trial);
    }
    Ok(trials)
}

/// Everything that would invalidate a cached measurement, as one comparable string.
///
/// The model, the runtime and the device list. A driver update is not covered and is the known gap:
/// it can change a timing without changing any of these, so the cache is a convenience, and
/// `SENTIN_DEVICE_PROBE=force` exists to ignore it.
#[must_use]
pub fn cache_key(model_xml: &Path, runtime_version: Option<&str>, available: &[String]) -> String {
    let size = std::fs::metadata(model_xml.with_extension("bin"))
        .map(|m| m.len())
        .unwrap_or(0);
    format!(
        "{}|{}|{}|{}",
        model_xml.display(),
        size,
        runtime_version.unwrap_or("unknown"),
        available.join(",")
    )
}

/// Where measurements are remembered between runs.
///
/// Deliberately not the model directory: a release bundle may sit somewhere read-only, and a
/// gateway that cannot write its cache must still start.
#[must_use]
pub fn cache_path() -> PathBuf {
    if let Ok(dir) = std::env::var("SENTIN_CACHE_DIR") {
        return PathBuf::from(dir).join("device-selection.json");
    }
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("sentin-npu").join("device-selection.json")
}

/// Cached measurements, keyed by [`cache_key`].
#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    entries: BTreeMap<String, Vec<Trial>>,
}

/// Read measurements remembered from an earlier run, if they still apply.
#[must_use]
pub fn cached(key: &str) -> Option<Vec<Trial>> {
    if std::env::var("SENTIN_DEVICE_PROBE").is_ok_and(|v| v.eq_ignore_ascii_case("force")) {
        return None;
    }
    let text = std::fs::read_to_string(cache_path()).ok()?;
    let file: CacheFile = serde_json::from_str(&text).ok()?;
    file.entries.get(key).cloned()
}

/// Remember measurements for the next start. Failure to write is not an error worth failing on.
pub fn remember(key: &str, trials: &[Trial]) {
    let path = cache_path();
    let mut file: CacheFile = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    file.entries.insert(key.to_string(), trials.to_vec());

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&file) {
        let _ = std::fs::write(&path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trial(device: &str, class: Class, steady_ms: f64) -> Trial {
        Trial {
            device: device.to_string(),
            full_name: None,
            class,
            compile_ms: Some(100.0),
            steady_ms: Some(steady_ms),
            error: None,
        }
    }

    #[test]
    fn a_device_over_the_ceiling_is_rejected_however_it_is_classified() {
        // The RTX 3060 case measured on 2026-08-31: enumerated, compiles, and hopeless.
        let trials = vec![
            trial("GPU", Class::DiscreteGpu, 224.8),
            trial("CPU", Class::Cpu, 8.7),
        ];
        let choice = rank(trials, Objective::Cost, 80.0);
        assert_eq!(choice.device.as_deref(), Some("CPU"));
        assert_eq!(choice.rejected.len(), 1);
        assert!(
            choice.rejected[0].reason.contains("ceiling"),
            "the reason must say it was too slow, not merely that it lost: {}",
            choice.rejected[0].reason
        );
    }

    #[test]
    fn the_old_cascade_would_have_picked_the_rejected_device() {
        // Guards the regression this module exists for: AUTO_ORDER puts GPU before CPU, so the
        // pre-2026-08-31 resolver chose the 224.8 ms device over the 8.7 ms one.
        let (cascade, _) = crate::ov::resolve_device(
            "AUTO",
            &["CPU".to_string(), "GPU".to_string()],
        );
        assert_eq!(cascade, "GPU", "the cascade is what we are replacing");

        let choice = rank(
            vec![
                trial("GPU", Class::DiscreteGpu, 224.8),
                trial("CPU", Class::Cpu, 8.7),
            ],
            Objective::Cost,
            80.0,
        );
        assert_ne!(choice.device.as_deref(), Some(cascade.as_str()));
    }

    #[test]
    fn cost_prefers_the_npu_over_a_faster_igpu_when_both_meet_the_ceiling() {
        // Core Ultra 7 258V, measured 2026-08-09: the iGPU is faster (2.7 vs 5.9 ms) and the NPU
        // is half the energy at 10 rps. With both inside the budget, cost is the tie-break that
        // the project's own numbers support.
        let trials = vec![
            trial("GPU", Class::IntegratedGpu, 2.7),
            trial("NPU", Class::Npu, 5.9),
            trial("CPU", Class::Cpu, 23.6),
        ];
        let choice = rank(trials, Objective::Cost, 80.0);
        assert_eq!(choice.device.as_deref(), Some("NPU"));
    }

    #[test]
    fn latency_takes_the_fastest_survivor_instead() {
        let trials = vec![
            trial("GPU", Class::IntegratedGpu, 2.7),
            trial("NPU", Class::Npu, 5.9),
            trial("CPU", Class::Cpu, 23.6),
        ];
        let choice = rank(trials, Objective::Latency, 80.0);
        assert_eq!(choice.device.as_deref(), Some("GPU"));
    }

    #[test]
    fn a_discrete_card_ranks_below_the_cpu_even_when_it_is_faster() {
        let trials = vec![
            trial("GPU", Class::DiscreteGpu, 5.0),
            trial("CPU", Class::Cpu, 9.0),
        ];
        assert_eq!(
            rank(trials.clone(), Objective::Cost, 80.0).device.as_deref(),
            Some("CPU")
        );
        assert_eq!(
            rank(trials, Objective::Latency, 80.0).device.as_deref(),
            Some("GPU"),
            "asking for latency must still get latency"
        );
    }

    #[test]
    fn a_refusal_is_reported_with_what_the_device_said() {
        let mut refused = trial("NPU", Class::Npu, 1.0);
        refused.error = Some("ZE_RESULT_ERROR_DEVICE_LOST".to_string());
        refused.steady_ms = None;
        let choice = rank(vec![refused, trial("CPU", Class::Cpu, 8.7)], Objective::Cost, 80.0);
        assert_eq!(choice.device.as_deref(), Some("CPU"));
        assert!(choice.rejected[0].reason.contains("ZE_RESULT_ERROR_DEVICE_LOST"));
    }

    #[test]
    fn the_ceiling_does_not_switch_layer_2_off_when_nothing_is_under_it() {
        // Advisory-first: a gateway inspecting slowly beats a gateway not inspecting. The ceiling
        // is there to prefer a better device, not to veto the only one.
        let choice = rank(
            vec![
                trial("GPU", Class::DiscreteGpu, 900.0),
                trial("CPU", Class::Cpu, 120.0),
            ],
            Objective::Cost,
            80.0,
        );
        assert_eq!(
            choice.device.as_deref(),
            Some("CPU"),
            "with nothing under the ceiling, take the fastest rather than nothing"
        );
        assert!(choice.over_ceiling, "and say so, loudly");
    }

    #[test]
    fn nothing_is_selected_when_no_device_will_run_the_model() {
        let mut refused = trial("CPU", Class::Cpu, 1.0);
        refused.error = Some("compile_model: out of memory".to_string());
        refused.steady_ms = None;
        let choice = rank(vec![refused], Objective::Cost, 80.0);
        assert!(choice.device.is_none(), "a hopeless machine must say so");
        assert!(!choice.over_ceiling, "refusing is not the same as being slow");
    }

    #[test]
    fn a_discrete_nvidia_card_is_recognised_without_a_device_type() {
        assert_eq!(
            classify("GPU", Some("NVIDIA GeForce RTX 3060 (dGPU)"), None),
            Class::DiscreteGpu
        );
        assert_eq!(
            classify("GPU", Some("Intel(R) Arc(TM) 140V GPU"), None),
            Class::IntegratedGpu
        );
        assert_eq!(
            classify("GPU", Some("NVIDIA GeForce RTX 3060"), Some("integrated")),
            Class::IntegratedGpu,
            "the plugin's own answer wins over name guessing"
        );
        assert_eq!(classify("NPU", None, None), Class::Npu);
        assert_eq!(classify("CPU", Some("Intel(R) Core(TM) i9-14900KF"), None), Class::Cpu);
    }

    #[test]
    fn objective_parses_the_words_an_operator_would_write() {
        assert_eq!(Objective::parse("cost"), Some(Objective::Cost));
        assert_eq!(Objective::parse("Energy"), Some(Objective::Cost));
        assert_eq!(Objective::parse("latency"), Some(Objective::Latency));
        assert_eq!(Objective::parse("whatever"), None);
    }

    #[test]
    fn the_cache_key_changes_with_the_device_list() {
        let path = Path::new("models/seq128/openvino_model.xml");
        let one = cache_key(path, Some("2026.3.0"), &["CPU".to_string()]);
        let two = cache_key(path, Some("2026.3.0"), &["CPU".to_string(), "GPU".to_string()]);
        assert_ne!(one, two, "a new device must invalidate the measurement");
    }
}
