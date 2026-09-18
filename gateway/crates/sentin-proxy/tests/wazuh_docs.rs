// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! The Wazuh deployment material must stay internally consistent.
//!
//! Three lists describe the same integration and nothing made them agree: the rules on the manager,
//! the guides an administrator reads, and the sample events they paste into `wazuh-logtest`. This
//! is the same shape of failure as a detector present in the code and absent from the configuration
//! - every part looks healthy on its own, and the deployment is quietly half-built.
//!
//! What each check prevents, in the order they are written:
//!
//! 1. **A rule nobody documented.** Rule 100524 was dead from the day it was written because
//!    `attachment_skipped` went into the gateway and not into the parent rule's event list. Nothing
//!    fired, nothing errored, and the dashboard looked exactly as it does when a system is quiet.
//! 2. **An event kind the parent rule does not accept**, which is the same failure one level down:
//!    a sample line that matches nothing teaches an administrator to distrust a working ruleset.
//! 3. **A translation that fell behind.** Two guides in two languages are two copies of one list of
//!    example files, and a file added to `examples/` after the guides were written would be
//!    documented in neither.
//!
//! The files are read from disk rather than pulled in with `include_str!` because `examples/` is a
//! directory: a hard-coded list here would be the fourth copy of the thing being checked.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The repository root, derived from this crate rather than from the working directory - `cargo
/// test` can be run from anywhere.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("the repository root must exist")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn guides() -> Vec<(&'static str, String)> {
    let root = repo_root();
    vec![
        (
            "deployment-en.md",
            read(&root.join("docs/wazuh/deployment-en.md")),
        ),
        (
            "wdrozenie-pl.md",
            read(&root.join("docs/wazuh/wdrozenie-pl.md")),
        ),
    ]
}

/// Every rule the manager would load has to appear in both guides.
///
/// A rule an administrator has never read about is a rule they cannot act on when it fires at level
/// 12 at three in the morning, and one they will not miss when it stops firing.
#[test]
fn every_rule_is_documented_in_both_languages() {
    let rules = read(&repo_root().join("packaging/wazuh/sentin_npu_rules.xml"));

    let ids: BTreeSet<&str> = rules
        .lines()
        .filter_map(|line| line.split_once("<rule id=\""))
        .filter_map(|(_, rest)| rest.split_once('"'))
        .map(|(id, _)| id)
        .collect();

    assert!(
        ids.len() >= 18,
        "expected the shipped ruleset, found {} ids: {ids:?}",
        ids.len()
    );

    for (name, text) in guides() {
        let missing: Vec<&&str> = ids.iter().filter(|id| !text.contains(**id)).collect();
        assert!(
            missing.is_empty(),
            "{name} documents no rule {missing:?}; a rule nobody reads about is a rule nobody acts on"
        );
    }
}

/// Every event kind in the sample file must be one the parent rule accepts.
///
/// Rule 100500 is the anchor every other rule hangs off through `if_sid`. An event kind missing
/// from its `event` field matches nothing at all, and a child whose parent never matches never
/// fires - silently, which is the entire problem with it.
#[test]
fn sample_events_match_the_parent_rule() {
    let root = repo_root();
    let samples = read(&root.join("docs/wazuh/examples/sample-audit.jsonl"));
    let rules = read(&root.join("packaging/wazuh/sentin_npu_rules.xml"));

    // The parent's own line, not the whole file: a child rule matching one kind would otherwise
    // satisfy this check on the parent's behalf.
    let anchor = rules
        .lines()
        .find(|line| line.contains("<field name=\"event\">^pii_detected$"))
        .expect("rule 100500 must list the event kinds it anchors");

    let mut seen = BTreeSet::new();
    for (n, line) in samples.lines().enumerate() {
        let event: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("sample-audit.jsonl line {}: {err}", n + 1));
        let kind = event["event"]
            .as_str()
            .unwrap_or_else(|| panic!("sample-audit.jsonl line {} has no event field", n + 1))
            .to_string();
        assert!(
            anchor.contains(&format!("^{kind}$")),
            "rule 100500 does not accept `{kind}`, so the sample on line {} matches nothing",
            n + 1
        );
        seen.insert(kind);
    }

    // Not merely valid: the file exists to exercise the ruleset, so a sample that covers only
    // detections would let the lifecycle rules ship untested.
    for required in ["pii_detected", "decision_made", "attachment_skipped"] {
        assert!(
            seen.contains(required),
            "the sample file carries no `{required}` event, so nothing tests the rules for it"
        );
    }
}

/// Both guides must name every example file.
///
/// They are translations of one document, and a file added to `examples/` afterwards is documented
/// in neither unless somebody remembers both. This is what "kept in step" has to mean to be worth
/// writing down.
#[test]
fn both_guides_name_every_example_file() {
    let dir = repo_root().join("docs/wazuh/examples");
    let entries = std::fs::read_dir(&dir).expect("docs/wazuh/examples must exist");

    let mut names: Vec<String> = entries
        .map(|entry| entry.expect("readable directory entry"))
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();

    assert!(
        names.len() >= 7,
        "expected the shipped example files, found {names:?}"
    );

    for (guide, text) in guides() {
        for name in &names {
            assert!(
                text.contains(name.as_str()),
                "{guide} never mentions examples/{name}"
            );
        }
    }
}
