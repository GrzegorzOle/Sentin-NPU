// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! OpenVINO device inventory and compile/inference probing.
//!
//! This is the foundation of the layer-2 bridge, and it is also what `sentin-gateway --doctor`
//! reports. The two uses share one requirement: **enumeration proves nothing.** A device that
//! appears in `available_devices` may still refuse to compile a real model, and on the NPU that is
//! the interesting case rather than an edge case. So every device here is asked to compile the
//! actual IR and run one inference, and the result is a fact rather than a capability claim.
//!
//! Deliberate limitation: the `openvino` crate at 0.11.0 exposes neither `query_model` nor
//! properties on a compiled model, so **operator-level fallback lists cannot be produced from
//! Rust**. `--doctor` answers "does it compile for this device, how long did that take, and does
//! it execute"; the per-operator breakdown belongs to the Python toolchain, whose OpenVINO
//! bindings do expose it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use openvino::{CompiledModel, Core, DeviceType, ElementType, PropertyKey, Tensor};
use serde::{Deserialize, Serialize};

/// Device preference order for `AUTO`: the whole point of the project is to prefer the NPU.
pub const AUTO_ORDER: [&str; 3] = ["NPU", "GPU", "CPU"];

/// Why the OpenVINO layer could not be used at all.
#[derive(Debug, thiserror::Error)]
pub enum OvError {
    /// The runtime could not be dlopen'd. The message spells out the symlink trap, because the
    /// error the C loader gives on its own names no cause.
    #[error(
        "OpenVINO runtime not loadable: {0}.\n\
         The crate links at run time (dlopen) and looks for *unversioned* sonames. An OpenVINO\n\
         Python wheel ships only versioned ones, so `libopenvino_c.so` may need a symlink and the\n\
         directory must be on LD_LIBRARY_PATH."
    )]
    Runtime(String),
    /// No IR at the given path. Models are gitignored and ship through releases, so a fresh clone
    /// has none until the toolchain has run.
    #[error("no IR found at {0} — run tools/prepare_model.py first")]
    NoModel(PathBuf),
}

/// What one device did when asked to do real work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceReport {
    /// OpenVINO's name for the device, as passed to `compile_model` — `CPU`, `GPU`, `NPU`.
    pub device: String,
    /// The device's own description of itself, e.g. `Intel(R) Core(TM) Ultra`. The field that
    /// tells a reader whether `GPU` meant an integrated or a discrete card.
    pub full_name: Option<String>,
    /// Precisions and features the plugin advertises, e.g. `FP32 INT8 EXPORT_IMPORT`. Advertised,
    /// not proven — which is why this struct also records what happened when the model was run.
    pub capabilities: Option<String>,
    /// Whether the plugin calls itself integrated or discrete.
    pub device_type: Option<String>,
    /// Architecture string, useful mainly for telling GPU vendors apart (`vendor=0x10de`).
    pub architecture: Option<String>,
    /// None when no IR was available to try.
    pub compiles: Option<bool>,
    /// Time to compile the graph for this device. On an NPU this is where an unsupported operator
    /// shows up, either as a fallback or as a refusal.
    pub compile_ms: Option<f64>,
    /// First inference is reported separately: on the NPU it includes graph setup and is often
    /// far slower than steady state, which is exactly what a deployment needs to know.
    pub first_infer_ms: Option<f64>,
    /// Median inference once warm — the number that belongs in a latency budget.
    pub steady_infer_ms: Option<f64>,
    /// Why the device refused, when it did. A refusal is a result worth reporting, not a gap.
    pub error: Option<String>,
}

/// Everything `--doctor` knows about this machine's OpenVINO stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// The OpenVINO build that was actually loaded, not the one the build expected.
    pub runtime_version: Option<String>,
    /// Everything `Core::available_devices` enumerated.
    pub available_devices: Vec<String>,
    /// Whether an NPU appears among them. False on every AMD machine, which is the constraint the
    /// whole project is arranged around.
    pub npu_present: bool,
    /// The IR that was compiled and run, if one was given. Without it the report is enumeration
    /// only, and enumeration proves nothing about whether the model runs.
    pub model_probed: Option<String>,
    /// One entry per enumerated device.
    pub devices: Vec<DeviceReport>,
    /// Human-readable observations for the person reading the report, e.g. why no NPU was found.
    pub notes: Vec<String>,
}

/// Enumerate devices and, when an IR is available, compile and run it on each.
///
/// # Errors
/// [`OvError::Runtime`] when the OpenVINO shared libraries cannot be loaded at all.
pub fn probe(model_xml: Option<&Path>) -> Result<Report, OvError> {
    let mut core = Core::new().map_err(|err| OvError::Runtime(err.to_string()))?;

    let available: Vec<String> = core
        .available_devices()
        .map_err(|err| OvError::Runtime(err.to_string()))?
        .iter()
        .map(ToString::to_string)
        .collect();

    let runtime_version = available
        .first()
        .and_then(|first| core.versions(first).ok())
        .and_then(|versions| versions.first().map(|(_, v)| v.build_number.clone()));

    let mut notes = Vec::new();
    let npu_present = available.iter().any(|d| d.starts_with("NPU"));
    if !npu_present {
        notes.push(
            "No NPU device. On Intel hardware this means a missing or mismatched NPU driver; on \
             AMD or older Intel parts it is expected."
                .to_string(),
        );
    }

    let mut devices = Vec::new();
    for name in &available {
        devices.push(probe_device(&mut core, name, model_xml, &mut notes));
    }

    if model_xml.is_none() {
        notes.push(
            "No IR supplied, so only enumeration was performed. Enumeration proves nothing about \
             whether a model will compile — run tools/prepare_model.py and re-run --doctor."
                .to_string(),
        );
    }
    notes.push(
        "Operator-level fallback lists are not available from Rust (the openvino crate exposes \
         neither query_model nor compiled-model properties). Run tools/query_ops.py for that."
            .to_string(),
    );

    Ok(Report {
        runtime_version,
        available_devices: available,
        npu_present,
        model_probed: model_xml.map(|p| p.display().to_string()),
        devices,
        notes,
    })
}

fn probe_device(
    core: &mut Core,
    name: &str,
    model_xml: Option<&Path>,
    notes: &mut Vec<String>,
) -> DeviceReport {
    let device = DeviceType::from(name);
    let property = |core: &Core, key: PropertyKey| core.get_property(&device, &key).ok();

    let mut report = DeviceReport {
        device: name.to_string(),
        full_name: property(core, PropertyKey::DeviceFullName),
        capabilities: property(core, PropertyKey::DeviceCapabilities),
        device_type: property(core, PropertyKey::Other("DEVICE_TYPE".into())),
        architecture: property(core, PropertyKey::Other("DEVICE_ARCHITECTURE".into())),
        compiles: None,
        compile_ms: None,
        first_infer_ms: None,
        steady_infer_ms: None,
        error: None,
    };

    let Some(xml) = model_xml else {
        return report;
    };
    let bin = xml.with_extension("bin");

    match compile_and_run(core, name, xml, &bin) {
        Ok(timing) => {
            report.compiles = Some(true);
            report.compile_ms = Some(timing.compile.as_secs_f64() * 1000.0);
            report.first_infer_ms = Some(timing.first.as_secs_f64() * 1000.0);
            report.steady_infer_ms = Some(timing.steady.as_secs_f64() * 1000.0);
        }
        Err(err) => {
            report.compiles = Some(false);
            report.error = Some(err.clone());
            // A refusal is a result, not a failure of the run — it is precisely what Phase 5 is
            // trying to find out, so it belongs in the report rather than aborting it.
            notes.push(format!("{name} refused the model: {err}"));
        }
    }
    report
}

struct Timing {
    compile: Duration,
    first: Duration,
    steady: Duration,
}

/// Build one **initialised** input tensor per declared input of `compiled`.
///
/// `Tensor::new` allocates without initialising: `ov_tensor_create` hands back whatever was already
/// in that memory. Fed to a NER graph, that is a block of token ids far outside the vocabulary, the
/// embedding gather indexes out of bounds, and what happens next is device-dependent — CPU and GPU
/// absorb it silently, the NPU hangs and Level Zero reports `ZE_RESULT_ERROR_DEVICE_LOST`. In a
/// report that is indistinguishable from the NPU refusing the model, and Phase 5 hit exactly that
/// false negative on a Core Ultra 7 258V whose NPU in fact runs both IR variants. The inputs are
/// therefore written explicitly rather than assumed to arrive zeroed.
///
/// `attention_mask` is filled with ones, not zeros. An all-zero mask masks every position out,
/// which real inference never produces, and a probe is only worth something if it exercises the
/// kernels the way the gateway will.
///
/// # Errors
/// Returns the OpenVINO message when the compiled model will not describe or allocate its inputs.
pub fn probe_inputs(compiled: &mut CompiledModel) -> Result<Vec<Tensor>, String> {
    let count = compiled
        .get_input_size()
        .map_err(|err| format!("get_input_size: {err}"))?;

    let mut tensors = Vec::with_capacity(count);
    for index in 0..count {
        let node = compiled
            .get_input_by_index(index)
            .map_err(|err| format!("get_input_by_index({index}): {err}"))?;
        let shape = node
            .get_shape()
            .map_err(|err| format!("get_shape({index}): {err}"))?;
        let element = node
            .get_element_type()
            .map_err(|err| format!("get_element_type({index}): {err}"))?;
        let name = node.get_name().unwrap_or_default();

        let mut tensor =
            Tensor::new(element, &shape).map_err(|err| format!("tensor alloc({index}): {err}"))?;
        tensor
            .get_raw_data_mut()
            .map_err(|err| format!("tensor data({index}): {err}"))?
            .fill(0);
        if name.contains("attention_mask") {
            fill_ones(&mut tensor, element)
                .map_err(|err| format!("attention_mask fill({index}): {err}"))?;
        }
        tensors.push(tensor);
    }
    Ok(tensors)
}

/// Set every element of `tensor` to one, for the integer types a mask is ever declared as.
///
/// An unexpected element type leaves the zeros in place rather than guessing at a byte pattern:
/// a wrong guess would be a silent lie about what was fed to the device.
fn fill_ones(tensor: &mut Tensor, element: ElementType) -> Result<(), String> {
    match element {
        ElementType::I64 => tensor.get_data_mut::<i64>().map(|data| data.fill(1)),
        ElementType::I32 => tensor.get_data_mut::<i32>().map(|data| data.fill(1)),
        _ => return Ok(()),
    }
    .map_err(|err| err.to_string())
}

fn compile_and_run(
    core: &mut Core,
    device: &str,
    xml: &Path,
    bin: &Path,
) -> Result<Timing, String> {
    let model = core
        .read_model_from_file(&xml.to_string_lossy(), &bin.to_string_lossy())
        .map_err(|err| format!("read_model: {err}"))?;

    let started = Instant::now();
    let mut compiled = core
        .compile_model(&model, DeviceType::from(device))
        .map_err(|err| format!("compile_model: {err}"))?;
    let compile = started.elapsed();

    // Shapes and element types come from the **compiled** model, not the source one. Asking the
    // source model for a concrete shape fails with "to_shape was called on a dynamic shape" even
    // when the IR was reshaped to static dimensions, because the port there is still described
    // partially. Compilation is what resolves it.
    let inputs = probe_inputs(&mut compiled)?;

    let mut request = compiled
        .create_infer_request()
        .map_err(|err| format!("create_infer_request: {err}"))?;

    // Layer 2 will feed real tokens; here the question is only whether the graph runs on this
    // device at all, and a valid synthetic batch exercises the same kernels.
    for (index, tensor) in inputs.iter().enumerate() {
        request
            .set_input_tensor_by_index(index, tensor)
            .map_err(|err| format!("set_input_tensor({index}): {err}"))?;
    }

    let started = Instant::now();
    request.infer().map_err(|err| format!("infer: {err}"))?;
    let first = started.elapsed();

    // Steady state, after the first call has paid for graph setup and any lazy allocation.
    let started = Instant::now();
    const STEADY_RUNS: u32 = 5;
    for _ in 0..STEADY_RUNS {
        request
            .infer()
            .map_err(|err| format!("infer (steady): {err}"))?;
    }
    let steady = started.elapsed() / STEADY_RUNS;

    Ok(Timing {
        compile,
        first,
        steady,
    })
}

/// Run inference on `device` as fast as possible for `duration`, returning how many completed.
///
/// This is the loop behind the per-device power comparison: energy per inference only means
/// something if the inferences are counted, and a device that is merely idle-waiting would
/// otherwise look wonderfully efficient.
///
/// # Errors
/// Returns the device's own refusal message when the model will not compile or run there.
pub fn run_for(xml: &Path, device: &str, duration: Duration) -> Result<(u64, Duration), String> {
    run_at_rate(xml, device, duration, None)
}

/// Run inference on `device` for `duration`, either saturating it or holding a fixed request rate.
///
/// `target_rps` of `None` means saturation. A rate is what makes the "what does this cost in
/// practice" question answerable — a gateway in front of one interactive agent does not saturate
/// anything — but be ready for the answer to be *unmeasurable*: at one request per second an
/// inference costing tens of millijoules amounts to a few tens of milliwatts, and a laptop
/// package's own drift is an order of magnitude larger. The caller is expected to compare the
/// result against a noise floor and say so, rather than print a number that looks like a
/// measurement.
///
/// # Errors
/// Returns the device's own refusal message when the model will not compile or run there, or a
/// message when `target_rps` is not positive.
pub fn run_at_rate(
    xml: &Path,
    device: &str,
    duration: Duration,
    target_rps: Option<f64>,
) -> Result<(u64, Duration), String> {
    let mut core = Core::new().map_err(|err| err.to_string())?;
    let bin = xml.with_extension("bin");
    let model = core
        .read_model_from_file(&xml.to_string_lossy(), &bin.to_string_lossy())
        .map_err(|err| format!("read_model: {err}"))?;
    let mut compiled = core
        .compile_model(&model, DeviceType::from(device))
        .map_err(|err| format!("compile_model: {err}"))?;

    let tensors = probe_inputs(&mut compiled)?;

    let mut request = compiled
        .create_infer_request()
        .map_err(|err| format!("create_infer_request: {err}"))?;
    for (index, tensor) in tensors.iter().enumerate() {
        request
            .set_input_tensor_by_index(index, tensor)
            .map_err(|err| format!("set_input_tensor: {err}"))?;
    }

    // One warm-up outside the measured window: the first call pays for graph setup, which on an
    // NPU can dwarf steady state and would otherwise be smeared across the average.
    request.infer().map_err(|err| format!("infer: {err}"))?;

    let started = Instant::now();
    let mut count = 0u64;
    match target_rps {
        // Saturation: as fast as the device will go. This is the only load at which a device's
        // energy can be attributed at all — see the note on the paced arm below.
        None => {
            while started.elapsed() < duration {
                request
                    .infer()
                    .map_err(|err| format!("infer (loop): {err}"))?;
                count += 1;
            }
        }
        // A fixed request rate, which is what a gateway in front of one interactive agent actually
        // sees. Each inference is scheduled against the start of the run rather than against the
        // previous one, so a slow call does not push the whole schedule later and quietly turn the
        // measurement back into saturation.
        Some(rps) if rps > 0.0 => {
            let interval = Duration::from_secs_f64(1.0 / rps);
            while started.elapsed() < duration {
                let due = interval.saturating_mul(u32::try_from(count).unwrap_or(u32::MAX));
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    std::thread::sleep(wait);
                }
                if started.elapsed() >= duration {
                    break;
                }
                request
                    .infer()
                    .map_err(|err| format!("infer (paced): {err}"))?;
                count += 1;
            }
        }
        Some(_) => return Err("target rate must be positive".to_string()),
    }
    Ok((count, started.elapsed()))
}

/// Resolve a requested device to one that is actually present.
///
/// `AUTO` walks [`AUTO_ORDER`], which is NPU first by design. Returns the chosen name and whether
/// a fallback happened, because "which device actually executed" is a fact this project logs
/// rather than assumes.
#[must_use]
pub fn resolve_device(requested: &str, available: &[String]) -> (String, bool) {
    let has = |needle: &str| available.iter().any(|d| d.starts_with(needle));

    if requested.eq_ignore_ascii_case("AUTO") {
        for candidate in AUTO_ORDER {
            if has(candidate) {
                return (candidate.to_string(), candidate != AUTO_ORDER[0]);
            }
        }
        return ("CPU".to_string(), true);
    }
    if has(requested) {
        return (requested.to_string(), false);
    }
    // An unavailable explicit request falls back rather than failing: an unsupported device must
    // not take the gateway down, it must be reported.
    for candidate in AUTO_ORDER {
        if has(candidate) {
            return (candidate.to_string(), true);
        }
    }
    ("CPU".to_string(), true)
}

/// The devices to try, in order, for a request — the resolved choice first, then the rest.
///
/// Enumeration says a device exists, not that it will compile the model, so the caller needs
/// somewhere to go when the first choice refuses. Only devices this machine actually has are
/// listed, and no device appears twice.
#[must_use]
pub fn device_candidates(requested: &str, available: &[String]) -> (Vec<String>, bool) {
    let (first, fell_back) = resolve_device(requested, available);
    let mut candidates = vec![first.clone()];
    candidates.extend(
        AUTO_ORDER
            .iter()
            .filter(|name| **name != first)
            .filter(|name| available.iter().any(|have| have.starts_with(**name)))
            .map(|name| (*name).to_string()),
    );
    (candidates, fell_back)
}

/// Default location of the IR the gateway would load.
#[must_use]
pub fn default_model_xml(repo_root: &Path, model: &str, precision: &str, seq: u32) -> PathBuf {
    repo_root
        .join("models")
        .join(model)
        .join(precision)
        .join(format!("seq{seq}"))
        .join("openvino_model.xml")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn auto_prefers_the_npu() {
        let (device, fell_back) = resolve_device("AUTO", &devices(&["CPU", "GPU", "NPU"]));
        assert_eq!(device, "NPU");
        assert!(
            !fell_back,
            "reaching the preferred device is not a fallback"
        );
    }

    #[test]
    fn auto_falls_back_in_order_when_the_npu_is_absent() {
        let (device, fell_back) = resolve_device("AUTO", &devices(&["CPU", "GPU"]));
        assert_eq!(device, "GPU");
        assert!(fell_back);

        let (device, fell_back) = resolve_device("AUTO", &devices(&["CPU"]));
        assert_eq!(device, "CPU");
        assert!(fell_back);
    }

    #[test]
    fn an_explicit_available_device_is_honoured() {
        let (device, fell_back) = resolve_device("CPU", &devices(&["CPU", "NPU"]));
        assert_eq!(
            device, "CPU",
            "an explicit request is not overridden by AUTO order"
        );
        assert!(!fell_back);
    }

    #[test]
    fn requesting_an_absent_device_falls_back_rather_than_failing() {
        // The dev machine has no NPU; asking for one must degrade, not take the gateway down.
        let (device, fell_back) = resolve_device("NPU", &devices(&["CPU", "GPU"]));
        assert_eq!(device, "GPU");
        assert!(fell_back, "the fallback must be visible to the caller");
    }

    #[test]
    fn device_ids_with_suffixes_still_match() {
        // OpenVINO reports multi-adapter systems as GPU.0, GPU.1 and so on.
        let (device, _) = resolve_device("AUTO", &devices(&["CPU", "GPU.0", "GPU.1"]));
        assert_eq!(device, "GPU");
    }

    #[test]
    fn candidates_put_the_resolved_device_first_and_keep_the_rest_as_escape_routes() {
        let (candidates, fell_back) = device_candidates("AUTO", &devices(&["CPU", "GPU", "NPU"]));
        assert_eq!(candidates, ["NPU", "GPU", "CPU"]);
        assert!(!fell_back);

        // An explicit request is honoured first, but the others stay reachable: a device that
        // enumerates can still refuse to compile, and that must not cost the gateway layer 2.
        let (candidates, _) = device_candidates("CPU", &devices(&["CPU", "GPU", "NPU"]));
        assert_eq!(candidates, ["CPU", "NPU", "GPU"]);
    }

    #[test]
    fn candidates_never_offer_a_device_the_machine_does_not_have() {
        let (candidates, fell_back) = device_candidates("NPU", &devices(&["CPU"]));
        assert_eq!(candidates, ["CPU"], "no phantom devices to fail over to");
        assert!(fell_back);
    }

    #[test]
    fn model_path_matches_the_toolchain_layout() {
        let path = default_model_xml(Path::new("/repo"), "herbert", "int8", 128);
        assert!(
            path.ends_with("models/herbert/int8/seq128/openvino_model.xml"),
            "{path:?}"
        );
    }
}
