// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! The three configurations this project ships must list the same detectors.
//!
//! There is `config/default.yaml` for a source build, the detector block the Windows installer
//! writes into `config.yaml`, and the one the AppImage writes under `--setup`. Three hand-written
//! copies of one list, and nothing made them agree.
//!
//! The failure this prevents does not look like a failure. **A detector present in the code and
//! absent from the configuration defaults to observing** - deliberately, because code must never
//! start rewriting somebody's traffic on its own - so an omission produces a gateway that finds the
//! identifier, writes an audit event about it, and forwards it anyway. That is precisely what
//! happened when `vat_eu` was added to `config/default.yaml` and to neither installer: on Windows a
//! fresh installation would have masked a Polish NIP and passed a Czech VAT number through, with
//! every check reporting success.
//!
//! These files are pulled in with `include_str!`, so moving one breaks the build rather than
//! quietly narrowing what is compared.

use std::collections::BTreeSet;

const DEFAULT_YAML: &str = include_str!("../../../../config/default.yaml");
const WINDOWS_INSTALLER: &str = include_str!("../../../../packaging/windows/sentin-npu.iss");
const LINUX_APPRUN: &str = include_str!("../../../../packaging/linux/AppRun");

/// Every detector key in a file, whatever syntax carries it.
///
/// One rule for all three: a detector is a line whose key is followed by `{ layer:`. That holds for
/// the YAML, for the shell heredoc inside `AppRun`, and for the Pascal string literals in the Inno
/// script, so the same reader works on a file that is not YAML at all.
fn detector_keys(source: &str) -> BTreeSet<String> {
    source
        .lines()
        .filter_map(|line| {
            // `split_once`, not `split`: `split` yields the whole line when the separator is
            // absent, so every line in the file would be offered as a detector.
            let (head, _) = line.split_once("{ layer:")?;
            // The key is the last word before the colon: `  nip:        ` in YAML,
            // `AddLine(Lines, N, '  nip:        ` in the installer.
            let key = head
                .trim_end()
                .strip_suffix(':')?
                .rsplit([' ', '\'', '(', ','])
                .next()?;
            if !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                Some(key.to_string())
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn every_shipped_configuration_lists_the_same_detectors() {
    let reference = detector_keys(DEFAULT_YAML);
    assert!(
        reference.len() >= 10,
        "the reader found only {reference:?} in config/default.yaml, which means it stopped \
         recognising the format rather than that the file shrank"
    );

    for (name, source) in [
        ("packaging/windows/sentin-npu.iss", WINDOWS_INSTALLER),
        ("packaging/linux/AppRun", LINUX_APPRUN),
    ] {
        let found = detector_keys(source);
        let missing: Vec<_> = reference.difference(&found).collect();
        let extra: Vec<_> = found.difference(&reference).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{name} does not match config/default.yaml.\n  missing: {missing:?}\n  unknown: \
             {extra:?}\nAn installed configuration without a detector does not fail - the detector \
             falls back to observing, so the identifier is found and forwarded."
        );
    }
}

#[test]
fn the_detectors_named_in_the_configuration_are_ones_the_gateway_knows() {
    // The other direction: a key nobody's `detector_key` produces is a line that will never match a
    // finding, and it would sit in the file looking like protection.
    let known: BTreeSet<String> = [
        sentin_core::DataKind::Pesel,
        sentin_core::DataKind::Nip,
        sentin_core::DataKind::VatEu,
        sentin_core::DataKind::Regon,
        sentin_core::DataKind::Iban,
        sentin_core::DataKind::PaymentCard,
        sentin_core::DataKind::Email,
        sentin_core::DataKind::PhonePl,
        sentin_core::DataKind::Person,
        sentin_core::DataKind::Organization,
        sentin_core::DataKind::Location,
    ]
    .into_iter()
    .map(|kind| sentin_proxy::config::detector_key(kind).to_string())
    .collect();

    let configured = detector_keys(DEFAULT_YAML);
    let unknown: Vec<_> = configured.difference(&known).collect();
    assert!(
        unknown.is_empty(),
        "config/default.yaml configures detectors the gateway has no data kind for: {unknown:?}"
    );

    let unconfigured: Vec<_> = known.difference(&configured).collect();
    assert!(
        unconfigured.is_empty(),
        "the gateway can produce these findings and the shipped configuration says nothing about \
         them, so they would only be observed: {unconfigured:?}"
    );
}
