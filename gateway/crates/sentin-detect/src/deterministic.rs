// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Layer 1: the deterministic detectors.
//!
//! A single left-to-right pass over the bytes, rather than several regex passes. Throughput is a
//! stated metric (M1, > 100 MB/s) because every proxied request pays this cost synchronously, and
//! one scan that classifies candidates by shape beats running seven independent patterns.
//!
//! Spans are byte offsets into the original text. Every pattern recognised here is ASCII, so byte
//! offsets always land on character boundaries even when the surrounding text is not ASCII.

use sentin_core::{DataKind, Finding, Layer, Validation};

use crate::checksums;

/// Longest digit sequence worth considering — the longest structured identifier is a 19-digit card.
const MAX_DIGITS: usize = 19;
/// Longest IBAN, per ISO 13616.
const MAX_IBAN: usize = 34;

/// Scan `text` for structured identifiers.
///
/// Findings are returned in order of appearance. A single stretch of text can yield more than one
/// finding of different kinds only when the shapes genuinely overlap; the scanner otherwise
/// advances past what it has matched.
#[must_use]
pub fn detect(text: &str) -> Vec<Finding> {
    let bytes = text.as_bytes();
    let mut findings = Vec::new();
    let mut index = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];

        if byte == b'@' {
            if let Some(span) = scan_email(bytes, index) {
                index = span.end;
                findings.push(finding(span, DataKind::Email, Validation::Pattern, 0.9));
                continue;
            }
            index += 1;
            continue;
        }

        if !starts_token(bytes, index) {
            index += 1;
            continue;
        }

        if byte.is_ascii_alphabetic() {
            if let Some(span) = scan_iban(bytes, index) {
                index = span.end;
                findings.push(finding(span, DataKind::Iban, Validation::Checksum, 1.0));
                continue;
            }
        }

        if byte.is_ascii_digit() || byte == b'+' {
            if let Some((span, kind, validation, confidence)) = scan_numeric(bytes, index) {
                index = span.end;
                findings.push(finding(span, kind, validation, confidence));
                continue;
            }
        }

        index += 1;
    }

    findings
}

fn finding(
    span: std::ops::Range<usize>,
    kind: DataKind,
    validation: Validation,
    confidence: f32,
) -> Finding {
    Finding {
        span,
        kind,
        confidence,
        layer: Layer::Deterministic,
        validation,
    }
}

/// True when position `index` begins a token, i.e. the previous byte is not alphanumeric.
///
/// Without this, an 11-digit window inside a 16-digit card number would be offered to the PESEL
/// detector, and roughly one card in ten would also be reported as a PESEL.
fn starts_token(bytes: &[u8], index: usize) -> bool {
    index == 0 || !bytes[index - 1].is_ascii_alphanumeric()
}

fn ends_token(bytes: &[u8], end: usize) -> bool {
    end >= bytes.len() || !bytes[end].is_ascii_alphanumeric()
}

/// Collect a run of digits that may contain single spaces or hyphens *between* digits.
///
/// Returns the digit values, the end offset, and whether any separator was seen — the latter
/// distinguishes a formatted phone number from a bare nine-digit REGON.
fn collect_digits(bytes: &[u8], start: usize) -> (Vec<u8>, usize, bool) {
    let mut digits = Vec::with_capacity(MAX_DIGITS);
    let mut end = start;
    let mut separated = false;

    while end < bytes.len() && digits.len() <= MAX_DIGITS {
        let byte = bytes[end];
        if byte.is_ascii_digit() {
            digits.push(byte - b'0');
            end += 1;
        } else if matches!(byte, b' ' | b'-')
            && end + 1 < bytes.len()
            && bytes[end + 1].is_ascii_digit()
            && !digits.is_empty()
        {
            separated = true;
            end += 1;
        } else {
            break;
        }
    }

    // A trailing separator is not part of the match.
    while end > start && matches!(bytes[end - 1], b' ' | b'-') {
        end -= 1;
    }
    (digits, end, separated)
}

type NumericMatch = (std::ops::Range<usize>, DataKind, Validation, f32);

fn scan_numeric(bytes: &[u8], start: usize) -> Option<NumericMatch> {
    // A leading +48 / 0048 makes this unambiguously a phone number.
    if let Some(m) = scan_phone_with_prefix(bytes, start) {
        return Some(m);
    }
    if bytes[start] == b'+' {
        return None;
    }

    let (digits, end, separated) = collect_digits(bytes, start);
    if !ends_token(bytes, end) || digits.is_empty() || digits.len() > MAX_DIGITS {
        return None;
    }

    let span = start..end;
    match digits.len() {
        11 if checksums::pesel(&digits) => Some((span, DataKind::Pesel, Validation::Checksum, 1.0)),
        10 if checksums::nip(&digits) => Some((span, DataKind::Nip, Validation::Checksum, 1.0)),
        9 | 14 if checksums::regon(&digits) => {
            Some((span, DataKind::Regon, Validation::Checksum, 1.0))
        }
        // Nine digits written as 123 456 789 or 123-456-789 read as a phone number, and there is
        // no checksum to appeal to. A bare nine-digit run is left to REGON above: guessing "phone"
        // from an unformatted number would fire on every order id in every prompt.
        9 if separated => Some((span, DataKind::PhonePl, Validation::Pattern, 0.6)),
        13..=19 if checksums::luhn(&digits) && card_prefix_is_known(&digits) => {
            Some((span, DataKind::PaymentCard, Validation::Checksum, 1.0))
        }
        _ => None,
    }
}

fn scan_phone_with_prefix(bytes: &[u8], start: usize) -> Option<NumericMatch> {
    let after_prefix = if bytes[start] == b'+' && bytes.get(start + 1..start + 3) == Some(b"48") {
        start + 3
    } else if bytes.get(start..start + 4) == Some(b"0048") {
        start + 4
    } else {
        return None;
    };

    let (digits, end, _) = collect_digits(bytes, after_prefix);
    if digits.len() == 9 && ends_token(bytes, end) {
        return Some((start..end, DataKind::PhonePl, Validation::Pattern, 0.8));
    }
    None
}

/// Issuer prefixes for the major card networks.
///
/// Luhn alone accepts about one random digit string in ten. Requiring a plausible issuer prefix as
/// well is what keeps M7 (zero false positives on PII-free text) attainable — the combination is
/// far more selective than either test alone.
fn card_prefix_is_known(digits: &[u8]) -> bool {
    let prefix = |n: usize| -> u32 {
        digits
            .iter()
            .take(n)
            .fold(0u32, |acc, d| acc * 10 + u32::from(*d))
    };
    let (len, p1, p2, p4) = (digits.len(), prefix(1), prefix(2), prefix(4));

    match p1 {
        4 => matches!(len, 13 | 16 | 19),        // Visa
        5 => matches!(p2, 51..=55) && len == 16, // Mastercard
        _ => match p2 {
            34 | 37 => len == 15,                                // American Express
            22..=27 if (2221..=2720).contains(&p4) => len == 16, // Mastercard 2-series
            65 => (16..=19).contains(&len),                      // Discover
            36 | 38 | 39 => (14..=19).contains(&len),            // Diners Club
            30 => (14..=19).contains(&len),                      // Diners Club carte blanche
            35 if (3528..=3589).contains(&p4) => (16..=19).contains(&len), // JCB
            60 if p4 == 6011 => (16..=19).contains(&len),        // Discover
            _ => false,
        },
    }
}

/// Expand around an `@` to recover an email address.
fn scan_email(bytes: &[u8], at: usize) -> Option<std::ops::Range<usize>> {
    let is_local =
        |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-');
    let is_domain = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-');

    let mut start = at;
    while start > 0 && is_local(bytes[start - 1]) {
        start -= 1;
    }
    // Leading dots or dashes are punctuation from the surrounding prose, not part of the address.
    while start < at && matches!(bytes[start], b'.' | b'-' | b'+') {
        start += 1;
    }

    let mut end = at + 1;
    while end < bytes.len() && is_domain(bytes[end]) {
        end += 1;
    }
    // A trailing dot is sentence punctuation.
    while end > at + 1 && bytes[end - 1] == b'.' {
        end -= 1;
    }

    let domain = &bytes[at + 1..end];
    let last_dot = domain.iter().rposition(|b| *b == b'.')?;
    let tld = &domain[last_dot + 1..];

    let plausible = start < at
        && tld.len() >= 2
        && tld.iter().all(u8::is_ascii_alphabetic)
        && !domain.starts_with(b".")
        && !domain.windows(2).any(|w| w == b"..");
    plausible.then_some(start..end)
}

/// Recognise an IBAN: `PL61 1090 1014 ...` or the same unspaced.
///
/// The grouping rules are load-bearing, not cosmetic. An earlier version accepted any
/// alphanumerics separated by single spaces, which let it swallow whole sentences: strip the
/// spaces from "of 5887 units left the depot at 14" and you get 34 characters that pass mod-97
/// about one time in ninety-seven. Requiring an uppercase country code and regular four-character
/// groups means ordinary prose cannot accidentally form an IBAN.
fn scan_iban(bytes: &[u8], start: usize) -> Option<std::ops::Range<usize>> {
    // Country code is two uppercase letters, then two check digits — canonical IBAN form.
    if !bytes.get(start)?.is_ascii_uppercase() || !bytes.get(start + 1)?.is_ascii_uppercase() {
        return None;
    }
    if !bytes.get(start + 2)?.is_ascii_digit() || !bytes.get(start + 3)?.is_ascii_digit() {
        return None;
    }

    let mut compact = Vec::with_capacity(MAX_IBAN);
    let mut end = start;
    let mut group_len = 0usize;
    let mut grouped = false;

    while end < bytes.len() && compact.len() < MAX_IBAN {
        let byte = bytes[end];
        if byte.is_ascii_alphanumeric() {
            compact.push(byte.to_ascii_uppercase());
            group_len += 1;
            end += 1;
        } else if byte == b' '
            && group_len == 4
            && bytes.get(end + 1).is_some_and(u8::is_ascii_alphanumeric)
        {
            // Only a completed four-character group may be followed by a space.
            grouped = true;
            group_len = 0;
            end += 1;
        } else {
            break;
        }
    }

    // In grouped form the final group may be short, but every earlier one had to be exactly four;
    // that is already enforced above, so only the trailing separator needs trimming.
    if grouped && group_len == 0 {
        end -= 1;
    }

    (ends_token(bytes, end) && checksums::iban(&compact)).then_some(start..end)
}
