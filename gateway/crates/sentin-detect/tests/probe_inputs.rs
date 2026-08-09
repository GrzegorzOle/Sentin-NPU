// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! The diagnostic probe must hand the device *initialised* inputs.
//!
//! This guards a fault that cost a whole session on borrowed Intel hardware. `Tensor::new` only
//! allocates — OpenVINO does not clear the buffer — so the probe used to feed whatever bytes
//! happened to be in that memory. As token ids those are far outside the vocabulary, the embedding
//! gather reads out of bounds, and the consequence splits by device: CPU and GPU absorb it, the NPU
//! hangs and Level Zero reports `ZE_RESULT_ERROR_DEVICE_LOST`. `--doctor` then printed
//! `compiles: NO` for an NPU that in fact runs the model, which is the worst possible failure for
//! the one artefact this project asks the community to attach to `npu-report` issues.
//!
//! The test cannot reproduce the hang without an NPU, so it asserts the property that prevents it:
//! every input byte the probe passes is one the code wrote on purpose.
//!
//! Skips, loudly, when the model or the OpenVINO libraries are absent — the normal state in CI.
//!
//! ```text
//! LD_LIBRARY_PATH=<openvino/libs> cargo test -p sentin-detect --test probe_inputs -- --nocapture
//! ```

use std::path::PathBuf;

use openvino::{Core, DeviceType, ElementType};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Read every element as `i64`/`i32`, whichever the port declares, so a mask can be compared
/// against a number rather than against a byte pattern.
fn values(tensor: &openvino::Tensor, element: ElementType) -> Vec<i64> {
    match element {
        ElementType::I64 => tensor.get_data::<i64>().expect("i64 data").to_vec(),
        ElementType::I32 => tensor
            .get_data::<i32>()
            .expect("i32 data")
            .iter()
            .map(|&v| i64::from(v))
            .collect(),
        other => panic!("unexpected input element type {other:?}"),
    }
}

#[test]
fn every_probe_input_is_written_before_it_reaches_the_device() {
    let dir = repo_root().join("models/herbert/int8/seq128");
    let xml = dir.join("openvino_model.xml");
    if !xml.exists() {
        println!(
            "SKIP: no IR at {} — run tools/prepare_model.py",
            xml.display()
        );
        return;
    }

    let Ok(mut core) = Core::new() else {
        println!("SKIP: OpenVINO runtime will not load");
        return;
    };
    let bin = xml.with_extension("bin");
    let model = core
        .read_model_from_file(&xml.to_string_lossy(), &bin.to_string_lossy())
        .expect("the IR parses");
    // CPU, because this asserts what the probe writes, not what any one device does with it.
    let mut compiled = core
        .compile_model(&model, DeviceType::CPU)
        .expect("the IR compiles for CPU");

    let tensors = sentin_detect::ov::probe_inputs(&mut compiled).expect("inputs are built");
    assert!(!tensors.is_empty(), "the model declares inputs");

    for (index, tensor) in tensors.iter().enumerate() {
        let node = compiled.get_input_by_index(index).expect("input port");
        let name = node.get_name().expect("input name");
        let element = node.get_element_type().expect("input element type");
        let data = values(tensor, element);
        assert!(!data.is_empty(), "input {name} has elements");

        if name.contains("attention_mask") {
            // Ones, not zeros: an all-zero mask masks every position out, which is a degenerate
            // input real inference never produces.
            assert!(
                data.iter().all(|&v| v == 1),
                "attention_mask must be all ones, found {:?}",
                &data[..data.len().min(8)]
            );
        } else {
            assert!(
                data.iter().all(|&v| v == 0),
                "input {name} must be zeroed, found {:?}",
                &data[..data.len().min(8)]
            );
        }
    }
    println!("checked {} probe inputs", tensors.len());
}
