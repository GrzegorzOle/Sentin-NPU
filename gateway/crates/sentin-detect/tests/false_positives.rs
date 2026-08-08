// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Metric M7: false positives on PII-free text. Threshold: zero, for checksum detectors.
//!
//! Two corpora, because they answer different questions.
//!
//! `pii_free_prose` is the one M7 is defined against: business text of the kind that actually
//! flows through an LLM gateway — dates, prices, quantities, order ids, times, version numbers.
//! A false positive here would block or mask a legitimate request.
//!
//! `random_digit_runs` is adversarial and is *not* expected to score zero. Checksums are
//! arithmetic, not magic: roughly one random nine-digit string in eleven satisfies the REGON
//! check. The test records the rate rather than pretending it is zero, because that rate is the
//! honest limit of what a checksum layer can promise.

use sentin_detect::{detect, DataKind, Validation};

/// Deterministic PRNG — reproducible failures are worth more than entropy here.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 11
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// At least `min_bytes` of business prose whose numbers are all ordinary, not identifiers.
fn pii_free_prose(min_bytes: usize) -> String {
    const SENTENCES: [&str; 8] = [
        "Zamówienie {n} zostało przyjęte {d} i przekazane do realizacji.",
        "Magazyn wydał {n} sztuk o godzinie {t}, pozostało {n} pozycji.",
        "Faktura na kwotę {a} zł została zaksięgowana {d}.",
        "Wersja {v} aplikacji poprawia wydajność o {p}%.",
        "The shipment of {n} units left the depot at {t} on {d}.",
        "Invoice total came to {a} EUR after the {p}% discount.",
        "Release {v} closes {n} open tickets reported since {d}.",
        "Kontrola jakości odrzuciła {n} z {n} partii w dniu {d}.",
    ];

    let mut rng = Rng(0x5EED);
    let mut out = String::with_capacity(min_bytes + 512);
    while out.len() < min_bytes {
        let template = SENTENCES[(rng.below(SENTENCES.len() as u64)) as usize];
        let mut sentence = String::with_capacity(template.len() + 24);
        for part in template.split_inclusive('}') {
            match part.rsplit_once('{') {
                Some((prefix, placeholder)) => {
                    sentence.push_str(prefix);
                    match placeholder {
                        "n}" => sentence.push_str(&rng.below(10_000).to_string()),
                        "d}" => sentence.push_str(&format!(
                            "{:04}-{:02}-{:02}",
                            2020 + rng.below(6),
                            1 + rng.below(12),
                            1 + rng.below(28)
                        )),
                        "t}" => {
                            sentence.push_str(&format!(
                                "{:02}:{:02}",
                                rng.below(24),
                                rng.below(60)
                            ));
                        }
                        "a}" => sentence.push_str(&format!(
                            "{},{:02}",
                            rng.below(100_000),
                            rng.below(100)
                        )),
                        "v}" => sentence.push_str(&format!(
                            "{}.{}.{}",
                            rng.below(10),
                            rng.below(20),
                            rng.below(30)
                        )),
                        _ => sentence.push_str(&rng.below(100).to_string()),
                    }
                }
                None => sentence.push_str(part),
            }
        }
        out.push_str(&sentence);
        out.push(' ');
    }
    out
}

#[test]
fn m7_no_false_positives_on_pii_free_prose() {
    let corpus = pii_free_prose(1024 * 1024);
    assert!(corpus.len() >= 1024 * 1024, "corpus must be at least 1 MB");

    let findings = detect(&corpus);
    let offenders: Vec<_> = findings
        .iter()
        .map(|f| (f.kind, &corpus[f.span.clone()]))
        .take(10)
        .collect();

    assert!(
        findings.is_empty(),
        "M7 violated: {} findings in {} bytes of PII-free prose, first: {:?}",
        findings.len(),
        corpus.len(),
        offenders
    );
}

/// Records the residual false-positive rate on adversarial input instead of asserting it away.
///
/// This is a characterisation test: it fails only if the rate becomes far worse than the
/// arithmetic predicts, which would mean a detector lost a guard (a length check, a token
/// boundary, an issuer prefix).
#[test]
fn adversarial_digit_runs_stay_within_the_arithmetic_limit() {
    const TRIALS: usize = 20_000;
    let mut rng = Rng(0xC0FFEE);
    let mut hits = 0usize;

    for _ in 0..TRIALS {
        let len = 9 + rng.below(11) as usize; // 9..=19 digits
        let mut number = String::with_capacity(len);
        for _ in 0..len {
            number.push(char::from(b'0' + (rng.below(10) as u8)));
        }
        if detect(&format!("kod {number} koniec"))
            .iter()
            .any(|f| f.validation == Validation::Checksum)
        {
            hits += 1;
        }
    }

    let rate = hits as f64 / TRIALS as f64;
    // Chance alone puts this near 5-8%: REGON and NIP are mod-11 checks, PESEL additionally has
    // to parse as a date, and cards need a known issuer prefix on top of Luhn.
    assert!(
        rate < 0.15,
        "checksum false-positive rate on random digit runs is {rate:.3} ({hits}/{TRIALS}) -- \
         a detector has probably lost a guard"
    );
    println!("adversarial checksum FP rate: {rate:.4} ({hits}/{TRIALS})");
}

#[test]
fn prose_without_any_digits_is_never_flagged() {
    let text = "Zespół potwierdził, że dokumentacja jest kompletna i gotowa do przekazania. \
                The reviewer confirmed that the documentation is complete."
        .repeat(200);
    assert!(detect(&text).is_empty());
    assert!(
        !text.contains(char::is_numeric),
        "corpus must be digit-free"
    );
    let _ = DataKind::Pesel; // keep the import meaningful if the assertions change
}
