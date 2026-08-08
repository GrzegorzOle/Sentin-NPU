// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! End-to-end behaviour of the layer-1 scanner.
//!
//! The exit criteria for this layer are absolute in both directions: every valid synthetic
//! identifier must be found, and a number whose checksum is wrong must never be reported. The
//! second half matters more — a false positive on a checksum detector can block a real request.

use proptest::prelude::*;
use sentin_detect::{detect, testdata, DataKind, Decision, Validation};

/// Every identifier the scanner reports, with the text it covers — spans must be usable directly.
fn found(text: &str) -> Vec<(DataKind, String)> {
    detect(text)
        .into_iter()
        .map(|f| (f.kind, text[f.span].to_string()))
        .collect()
}

fn kinds(text: &str) -> Vec<DataKind> {
    detect(text).into_iter().map(|f| f.kind).collect()
}

#[test]
fn finds_each_identifier_in_running_prose() {
    let pesel = testdata::pesel(1944, 5, 14, 135);
    let text = format!("Klient {pesel} złożył wniosek w poniedziałek.");
    assert_eq!(found(&text), vec![(DataKind::Pesel, pesel)]);
}

#[test]
fn spans_are_byte_offsets_that_slice_correctly_after_non_ascii() {
    // Polish text before the identifier means byte offsets and character offsets diverge; a span
    // computed in characters would slice mid-codepoint and panic, or silently point elsewhere.
    let pesel = testdata::pesel(1985, 3, 7, 42);
    let text = format!("Zażółć gęślą jaźń, numer {pesel}, koniec.");
    let findings = detect(&text);
    assert_eq!(findings.len(), 1);
    assert_eq!(&text[findings[0].span.clone()], pesel);
}

#[test]
fn recognises_every_supported_kind() {
    let nip = testdata::nip([1, 2, 3, 4, 5, 6, 3, 2, 1]).expect("valid base");
    let regon = testdata::regon9([1, 2, 3, 4, 5, 6, 7, 8]);
    let iban = testdata::iban("PL", "109010140000071219812874");
    let card = testdata::card("4", 16);

    assert_eq!(kinds(&format!("NIP {nip}")), vec![DataKind::Nip]);
    assert_eq!(kinds(&format!("REGON {regon}")), vec![DataKind::Regon]);
    assert_eq!(kinds(&format!("IBAN {iban}")), vec![DataKind::Iban]);
    assert_eq!(kinds(&format!("karta {card}")), vec![DataKind::PaymentCard]);
    assert_eq!(
        kinds("napisz na a.kowalska@example.com"),
        vec![DataKind::Email]
    );
    assert_eq!(kinds("tel. +48 123 456 789"), vec![DataKind::PhonePl]);
}

#[test]
fn accepts_grouped_formatting() {
    let iban = testdata::iban("PL", "109010140000071219812874");
    let grouped: String = iban
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).expect("ascii"))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(kinds(&format!("konto: {grouped}")), vec![DataKind::Iban]);

    let card = testdata::card("4", 16);
    let spaced = format!(
        "{} {} {} {}",
        &card[0..4],
        &card[4..8],
        &card[8..12],
        &card[12..16]
    );
    assert_eq!(
        kinds(&format!("karta {spaced}")),
        vec![DataKind::PaymentCard]
    );
}

#[test]
fn rejects_numbers_whose_checksum_is_wrong() {
    // Corrupt the final digit of each identifier; nothing may be reported.
    let mut cases = vec![
        testdata::pesel(1944, 5, 14, 135),
        testdata::nip([1, 2, 3, 4, 5, 6, 3, 2, 1]).expect("valid base"),
        testdata::regon9([1, 2, 3, 4, 5, 6, 7, 8]),
        testdata::card("4", 16),
    ];
    cases.push(testdata::iban("PL", "109010140000071219812874"));

    for original in cases {
        let corrupted = corrupt_last_digit(&original);
        let reported = kinds(&format!("wartość {corrupted} w tekście"));
        assert!(
            reported.is_empty(),
            "{corrupted} (from {original}) was reported as {reported:?}"
        );
    }
}

#[test]
fn does_not_find_a_pesel_inside_a_longer_number() {
    // An 11-digit window of a 16-digit card passes the PESEL checksum about one time in ten.
    // Token boundaries are what stop that from happening.
    for len in [13usize, 16, 19] {
        let card = testdata::card("4", len);
        let reported = kinds(&format!("karta {card}"));
        assert!(
            !reported.contains(&DataKind::Pesel),
            "{card} reported as PESEL"
        );
    }
}

#[test]
fn ignores_identifier_shaped_substrings_of_words() {
    let pesel = testdata::pesel(1944, 5, 14, 135);
    assert!(
        kinds(&format!("ref{pesel}")).is_empty(),
        "prefixed by letters"
    );
    assert!(
        kinds(&format!("{pesel}x")).is_empty(),
        "suffixed by letters"
    );
}

#[test]
fn email_and_phone_are_pattern_only_and_cannot_block() {
    for text in ["kontakt: biuro@firma.pl", "tel. +48 601 234 567"] {
        for finding in detect(text) {
            assert_eq!(finding.validation, Validation::Pattern, "{text}");
            assert_eq!(
                finding.clamp_decision(Decision::Blocked),
                Decision::Masked,
                "{text} must not be blockable"
            );
        }
    }
}

#[test]
fn checksum_backed_findings_may_block() {
    let pesel = testdata::pesel(1944, 5, 14, 135);
    let finding = detect(&pesel).pop().expect("detected");
    assert_eq!(finding.validation, Validation::Checksum);
    assert_eq!(finding.clamp_decision(Decision::Blocked), Decision::Blocked);
}

#[test]
fn clean_prose_produces_nothing() {
    // M7 in miniature: ordinary business text, including numbers that are not identifiers.
    let text = "Zamówienie 12345 z dnia 2026-08-08 obejmuje 42 sztuki w cenie 199,99 zł. \
                Magazyn 7 potwierdził wysyłkę o 14:35. Wersja 2.1.3 aplikacji.";
    assert!(detect(text).is_empty(), "{:?}", found(text));
}

#[test]
fn email_boundaries_exclude_surrounding_punctuation() {
    assert_eq!(
        found("Napisz do (jan.nowak@example.co.uk), proszę."),
        vec![(DataKind::Email, "jan.nowak@example.co.uk".to_string())]
    );
}

proptest! {
    /// Any well-formed PESEL is detected, whatever the date or serial.
    #[test]
    fn every_valid_pesel_is_detected(
        year in 1900u32..2030,
        month in 1u32..=12,
        day in 1u32..=28,
        serial in 0u32..10_000,
    ) {
        let pesel = testdata::pesel(year, month, day, serial);
        let text = format!("dane: {pesel} koniec");
        prop_assert_eq!(kinds(&text), vec![DataKind::Pesel], "{}", pesel);
    }

    /// Corrupting the check digit must always suppress the finding — never a "close enough" match.
    #[test]
    fn pesel_with_a_broken_check_digit_is_never_detected(
        year in 1900u32..2030,
        month in 1u32..=12,
        day in 1u32..=28,
        serial in 0u32..10_000,
    ) {
        let corrupted = corrupt_last_digit(&testdata::pesel(year, month, day, serial));
        let reported = kinds(&format!("dane: {corrupted} koniec"));
        prop_assert!(!reported.contains(&DataKind::Pesel), "{} -> {:?}", corrupted, reported);
    }

    /// Nine base digits either produce a valid NIP or none at all; whichever, detection agrees.
    #[test]
    fn nip_detection_matches_generation(base in prop::array::uniform9(0u8..=9)) {
        if let Some(nip) = testdata::nip(base) {
            prop_assert_eq!(kinds(&format!("NIP: {}", nip)), vec![DataKind::Nip]);
        }
    }

    /// Arbitrary digit runs must not be reported unless they genuinely validate.
    #[test]
    fn random_digit_runs_are_not_reported_as_cards(digits in "[0-9]{16}") {
        let reported = kinds(&format!("kod {digits}"));
        if reported.contains(&DataKind::PaymentCard) {
            // Only legitimate when both Luhn and a known issuer prefix agree.
            let values: Vec<u8> = digits.bytes().map(|b| b - b'0').collect();
            prop_assert!(sentin_detect::checksums::luhn(&values), "{}", digits);
        }
    }
}

fn corrupt_last_digit(value: &str) -> String {
    let mut bytes = value.as_bytes().to_vec();
    let last = bytes.len() - 1;
    bytes[last] = if bytes[last] == b'9' {
        b'0'
    } else {
        bytes[last] + 1
    };
    String::from_utf8(bytes).expect("ascii")
}
