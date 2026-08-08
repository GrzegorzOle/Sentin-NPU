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

use openvino::{Core, DeviceType, PropertyKey, Tensor};
use serde::{Deserialize, Serialize};

/// Device preference order for `AUTO`: the whole point of the project is to prefer the NPU.
pub const AUTO_ORDER: [&str; 3] = ["NPU", "GPU", "CPU"];

#[derive(Debug, thiserror::Error)]
pub enum OvError {
    #[error(
        "OpenVINO runtime not loadable: {0}.\n\
         The crate links at run time (dlopen) and looks for *unversioned* sonames. An OpenVINO\n\
         Python wheel ships only versioned ones, so `libopenvino_c.so` may need a symlink and the\n\
         directory must be on LD_LIBRARY_PATH."
    )]
    Runtime(String),
    #[error("no IR found at {0} — run tools/prepare_model.py first")]
    NoModel(PathBuf),
}

/// What one device did when asked to do real work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceReport {
    pub device: String,
    pub full_name: Option<String>,
    pub capabilities: Option<String>,
    pub device_type: Option<String>,
    pub architecture: Option<String>,
    /// None when no IR was available to try.
    pub compiles: Option<bool>,
    pub compile_ms: Option<f64>,
    /// First inference is reported separately: on the NPU it includes graph setup and is often
    /// far slower than steady state, which is exactly what a deployment needs to know.
    pub first_infer_ms: Option<f64>,
    pub steady_infer_ms: Option<f64>,
    pub error: Option<String>,
}

/// Everything `--doctor` knows about this machine's OpenVINO stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub runtime_version: Option<String>,
    pub available_devices: Vec<String>,
    pub npu_present: bool,
    pub model_probed: Option<String>,
    pub devices: Vec<DeviceReport>,
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
         neither query_model nor compiled-model properties). Use the Python toolchain for that."
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
    let input_count = compiled
        .get_input_size()
        .map_err(|err| format!("get_input_size: {err}"))?;

    let mut inputs = Vec::with_capacity(input_count);
    for index in 0..input_count {
        let node = compiled
            .get_input_by_index(index)
            .map_err(|err| format!("get_input_by_index({index}): {err}"))?;
        let shape = node
            .get_shape()
            .map_err(|err| format!("get_shape({index}): {err}"))?;
        let element = node
            .get_element_type()
            .map_err(|err| format!("get_element_type({index}): {err}"))?;
        inputs.push((shape, element));
    }

    let mut request = compiled
        .create_infer_request()
        .map_err(|err| format!("create_infer_request: {err}"))?;

    // Feed every declared input with zeros. Layer 2 will feed real tokens; here the question is
    // only whether the graph runs on this device at all, and zeros exercise the same kernels.
    for (index, (shape, element)) in inputs.iter().enumerate() {
        let tensor =
            Tensor::new(*element, shape).map_err(|err| format!("tensor alloc({index}): {err}"))?;
        request
            .set_input_tensor_by_index(index, &tensor)
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
    let mut core = Core::new().map_err(|err| err.to_string())?;
    let bin = xml.with_extension("bin");
    let model = core
        .read_model_from_file(&xml.to_string_lossy(), &bin.to_string_lossy())
        .map_err(|err| format!("read_model: {err}"))?;
    let mut compiled = core
        .compile_model(&model, DeviceType::from(device))
        .map_err(|err| format!("compile_model: {err}"))?;

    let inputs = compiled
        .get_input_size()
        .map_err(|err| format!("get_input_size: {err}"))?;
    let mut tensors = Vec::with_capacity(inputs);
    for index in 0..inputs {
        let node = compiled
            .get_input_by_index(index)
            .map_err(|err| format!("get_input_by_index: {err}"))?;
        let shape = node
            .get_shape()
            .map_err(|err| format!("get_shape: {err}"))?;
        let element = node
            .get_element_type()
            .map_err(|err| format!("get_element_type: {err}"))?;
        tensors.push(Tensor::new(element, &shape).map_err(|err| format!("tensor alloc: {err}"))?);
    }

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
    while started.elapsed() < duration {
        request
            .infer()
            .map_err(|err| format!("infer (loop): {err}"))?;
        count += 1;
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
    fn model_path_matches_the_toolchain_layout() {
        let path = default_model_xml(Path::new("/repo"), "herbert", "int8", 128);
        assert!(
            path.ends_with("models/herbert/int8/seq128/openvino_model.xml"),
            "{path:?}"
        );
    }
}
