// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Metric M4 from the Rust path, pinned to the Python reference.
//!
//! The quality figures published for this project were measured by `tools/validate_model.py`, but
//! the Rust engine is what actually ships. Two implementations of the same decoding rules are two
//! chances to get them wrong, and the ways they can differ are quiet ones: a span off by a
//! subword, a label taken from the wrong token, byte offsets treated as character offsets. None of
//! those crash — they just cost F1 that nobody notices.
//!
//! So this scores the Rust path over the same committed fixtures and asserts it lands where the
//! Python run does. If either side drifts, the test says so.
//!
//! Skips with a printed reason when the model or the OpenVINO libraries are absent.

use std::collections::HashSet;
use std::path::PathBuf;

use sentin_detect::ner::NerEngine;
use sentin_detect::DataKind;

/// Scores measured by `tools/validate_model.py --model herbert --seq 128 --precision int8
/// --dataset fixtures` on 2026-08-08. The Rust path must agree.
const PYTHON_F1_PL: f64 = 95.52;
const PYTHON_F1_EN: f64 = 84.44;
/// Both implementations decode the same logits with the same rules, so they should agree closely.
/// A percentage point of slack absorbs tie-breaking on equal scores without hiding a real bug.
const TOLERANCE_PP: f64 = 1.0;

struct Example {
    text: String,
    spans: Vec<(usize, usize, DataKind)>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Parse the `[TYPE:surface]` markup the fixtures are written in.
///
/// The same reasoning as on the Python side: offsets are derived from the markup rather than
/// maintained by hand, because hand-written offsets drift the moment a sentence is edited and a
/// fixture whose gold spans point at the wrong characters produces confident, wrong numbers.
fn parse_marked(marked: &str) -> Example {
    let mut text = String::with_capacity(marked.len());
    let mut spans = Vec::new();
    let mut rest = marked;

    while let Some(open) = rest.find('[') {
        let Some(colon) = rest[open..].find(':').map(|i| open + i) else {
            break;
        };
        let Some(close) = rest[colon..].find(']').map(|i| colon + i) else {
            break;
        };
        text.push_str(&rest[..open]);

        let kind = match &rest[open + 1..colon] {
            "PER" => DataKind::Person,
            "ORG" => DataKind::Organization,
            "LOC" => DataKind::Location,
            other => panic!("unknown entity type {other:?} in fixture"),
        };
        let surface = &rest[colon + 1..close];
        let start = text.len();
        text.push_str(surface);
        spans.push((start, text.len(), kind));

        rest = &rest[close + 1..];
    }
    text.push_str(rest);
    Example { text, spans }
}

fn load(lang: &str) -> Vec<Example> {
    let path = repo_root().join(format!("tests/fixtures/ner_{lang}.jsonl"));
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .map(|line| {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("each fixture line is JSON");
            parse_marked(value["marked"].as_str().expect("a `marked` field"))
        })
        .collect()
}

fn engine() -> Option<NerEngine> {
    let dir = repo_root().join("models/herbert/int8/seq128");
    if !dir.join("openvino_model.xml").exists() {
        println!(
            "SKIP: no IR at {} — run tools/prepare_model.py",
            dir.display()
        );
        return None;
    }
    match NerEngine::load(&dir, "CPU") {
        Ok(engine) => Some(engine),
        Err(err) => {
            println!("SKIP: engine will not load ({err})");
            None
        }
    }
}

/// Exact-span-match F1, the same scoring the Python reference applies.
fn score(engine: &mut NerEngine, examples: &[Example]) -> (f64, f64, f64) {
    let (mut hits, mut predicted, mut gold) = (0usize, 0usize, 0usize);
    for example in examples {
        let found: HashSet<(usize, usize, DataKind)> = engine
            .detect(&example.text)
            .expect("inference runs")
            .into_iter()
            .map(|f| (f.span.start, f.span.end, f.kind))
            .collect();
        let expected: HashSet<(usize, usize, DataKind)> = example.spans.iter().copied().collect();

        hits += found.intersection(&expected).count();
        predicted += found.len();
        gold += expected.len();
    }
    let precision = if predicted == 0 {
        0.0
    } else {
        hits as f64 / predicted as f64
    };
    let recall = if gold == 0 {
        0.0
    } else {
        hits as f64 / gold as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    (precision * 100.0, recall * 100.0, f1 * 100.0)
}

#[test]
fn m4_the_rust_path_scores_the_same_as_the_python_reference() {
    let Some(mut engine) = engine() else { return };

    for (lang, reference) in [("pl", PYTHON_F1_PL), ("en", PYTHON_F1_EN)] {
        let examples = load(lang);
        assert!(!examples.is_empty(), "no fixtures for {lang}");
        let (precision, recall, f1) = score(&mut engine, &examples);

        println!(
            "M4 {lang}: precision {precision:.2}  recall {recall:.2}  F1 {f1:.2}  \
             (python reference {reference:.2})"
        );
        assert!(
            (f1 - reference).abs() <= TOLERANCE_PP,
            "{lang}: Rust scores {f1:.2} against the Python reference {reference:.2}. \
             The two implementations have drifted — check subword aggregation, BIO merging and \
             byte-versus-character offsets before adjusting this constant."
        );
    }
}

#[test]
fn fixture_markup_parses_to_the_spans_it_annotates() {
    // The scorer is only as good as its gold data, so check the parser against a known sentence
    // before trusting anything it produces.
    let example = parse_marked("Pan [PER:Marek Nowak] z [LOC:Bydgoszczy] zadzwonil.");
    assert_eq!(example.text, "Pan Marek Nowak z Bydgoszczy zadzwonil.");
    assert_eq!(example.spans.len(), 2);
    assert_eq!(
        &example.text[example.spans[0].0..example.spans[0].1],
        "Marek Nowak"
    );
    assert_eq!(example.spans[0].2, DataKind::Person);
    assert_eq!(
        &example.text[example.spans[1].0..example.spans[1].1],
        "Bydgoszczy"
    );
}

#[test]
fn fixture_spans_survive_non_ascii_text() {
    // Byte offsets, and Polish text puts the two unit systems out of step immediately.
    let example = parse_marked("Zażółć gęślą [PER:Anna Zarembska] jaźń.");
    let (start, end, _) = example.spans[0];
    assert_eq!(&example.text[start..end], "Anna Zarembska");
}
