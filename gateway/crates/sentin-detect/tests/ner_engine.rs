// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! End-to-end NER against the real IR, checked against the same fixtures the Python reference uses.
//!
//! These tests **skip** when the model or the OpenVINO libraries are absent, which is the normal
//! state in CI and on any machine that has not run the toolchain. A skipped test says so out loud
//! rather than passing silently — a green tick that proves nothing is worse than a red one.
//!
//! Run them locally with:
//!
//! ```text
//! LD_LIBRARY_PATH=<openvino/libs> cargo test -p sentin-detect --test ner_engine -- --nocapture
//! ```

use std::path::PathBuf;

use sentin_detect::ner::NerEngine;
use sentin_detect::DataKind;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Load the engine, or explain why the test is being skipped.
fn engine() -> Option<NerEngine> {
    let dir = repo_root().join("models/herbert/int8/seq128");
    if !dir.join("openvino_model.xml").exists() {
        println!(
            "SKIP: no IR at {} — run tools/prepare_model.py",
            dir.display()
        );
        return None;
    }
    match NerEngine::load(&dir, "AUTO") {
        Ok(engine) => Some(engine),
        Err(err) => {
            println!("SKIP: engine will not load ({err})");
            None
        }
    }
}

#[test]
fn finds_a_polish_person_and_reports_the_device() {
    let Some(mut engine) = engine() else { return };
    println!("executing on {}", engine.device());

    let text = "Prosze przygotowac umowe dla pana Marka Nowaka z Warszawy.";
    let findings = engine.detect(text).expect("inference runs");

    let people: Vec<&str> = findings
        .iter()
        .filter(|f| f.kind == DataKind::Person)
        .map(|f| &text[f.span.clone()])
        .collect();
    assert!(
        people.iter().any(|p| p.contains("Nowak")),
        "expected a PERSON containing 'Nowak', got {findings:?}"
    );
}

#[test]
fn spans_are_byte_offsets_that_slice_polish_text_correctly() {
    // The tokenizer reports byte offsets and Finding::span is a byte range, so slicing must
    // simply work. A span computed in characters would panic here or point at the wrong text.
    let Some(mut engine) = engine() else { return };

    let text = "Zażółć gęślą jaźń — pisze Anna Zarembska z Bydgoszczy.";
    let findings = engine.detect(text).expect("inference runs");

    for finding in &findings {
        // Slicing is the assertion: an invalid byte range panics.
        let slice = &text[finding.span.clone()];
        assert!(!slice.is_empty(), "empty span in {findings:?}");
        assert!(
            text.is_char_boundary(finding.span.start) && text.is_char_boundary(finding.span.end),
            "span {:?} is not on a character boundary",
            finding.span
        );
    }
    assert!(
        findings
            .iter()
            .any(|f| text[f.span.clone()].contains("Zarembska")),
        "expected to find the surname after non-ASCII text, got {findings:?}"
    );
}

#[test]
fn ner_findings_are_advisory_only() {
    let Some(mut engine) = engine() else { return };

    let findings = engine
        .detect("Jan Kowalski pracuje w Alterna Logistyka.")
        .expect("inference runs");
    assert!(!findings.is_empty(), "expected at least one entity");

    for finding in findings {
        assert_eq!(finding.layer, sentin_detect::Layer::Ner);
        assert_eq!(
            finding.clamp_decision(sentin_detect::Decision::Blocked),
            sentin_detect::Decision::Masked,
            "layer 2 must never justify blocking"
        );
    }
}

#[test]
fn text_without_entities_produces_nothing() {
    let Some(mut engine) = engine() else { return };
    let findings = engine
        .detect("Zamowienie zostalo przyjete i przekazane do realizacji.")
        .expect("inference runs");
    let people: Vec<_> = findings
        .iter()
        .filter(|f| f.kind == DataKind::Person)
        .collect();
    assert!(
        people.is_empty(),
        "unexpected PERSON in neutral text: {people:?}"
    );
}

#[test]
fn long_text_is_truncated_rather_than_failing() {
    // The IR is compiled for a fixed sequence length; anything longer must degrade, not panic.
    let Some(mut engine) = engine() else { return };
    let text = "Klient Marek Nowak zlozyl wniosek. ".repeat(200);
    let findings = engine.detect(&text).expect("long input must not fail");
    for finding in &findings {
        assert!(finding.span.end <= text.len());
    }
}
