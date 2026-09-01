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
/// Longest national part of an EU VAT identification number - twelve, for the Netherlands and
/// Sweden. Bounding the scan keeps a long alphanumeric blob from being re-read at every offset.
const MAX_VAT_PART: usize = 12;

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
            // After IBAN, which starts the same way and is longer, so it cannot be stolen by this.
            if let Some((span, kind, validation, confidence)) = scan_vat_eu(bytes, index) {
                index = span.end;
                findings.push(finding(span, kind, validation, confidence));
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

/// A VAT identification number: two letters of country code and the national part behind it.
///
/// This exists because of how a tax number is actually written. An invoice says
/// `Identyfikator VAT: PL6511718003`, and the ten-digit NIP detector never saw it: a digit run with
/// letters glued to its front is not a token, and the token rule is what stops eleven digits inside
/// a sixteen-digit card being offered to the PESEL detector. Found on a real invoice that produced
/// only `organization` findings while its tax number went through untouched.
///
/// **A Polish number comes back as a [`DataKind::Nip`], not as a VAT number**, and carries
/// [`Validation::Checksum`] with it. `PL6511718003` and `6511718003` are the same identifier
/// written two ways, so they must produce the same finding and be blockable on the same evidence.
/// Anything else would let a policy be escaped by adding two letters.
///
/// Every other member state is shape only. Several validate with checksums this project does not
/// implement, and claiming arithmetic proof it does not have is exactly the false positive that
/// gets a DLP tool switched off - so these advise or mask, and the type system will not let them
/// block.
fn scan_vat_eu(bytes: &[u8], start: usize) -> Option<NumericMatch> {
    let country: [u8; 2] = bytes.get(start..start + 2)?.try_into().ok()?;
    if !country.iter().all(u8::is_ascii_uppercase) {
        return None;
    }

    let mut end = start + 2;
    while end < bytes.len()
        && bytes[end].is_ascii_alphanumeric()
        && end - (start + 2) <= MAX_VAT_PART
    {
        end += 1;
    }
    // A token longer than any member state's number is not one; the scan above stops one character
    // past the limit precisely so that this catches it.
    if !ends_token(bytes, end) {
        return None;
    }
    let part = &bytes[start + 2..end];

    if country == *b"PL" {
        if part.len() == 10 && part.iter().all(u8::is_ascii_digit) {
            let digits: Vec<u8> = part.iter().map(|byte| byte - b'0').collect();
            if checksums::nip(&digits) {
                return Some((start..end, DataKind::Nip, Validation::Checksum, 1.0));
            }
        }
        // Ten digits that fail the checksum are not a NIP, exactly as when they are written bare.
        return None;
    }

    if vat_national_part_is_valid(&country, part) {
        return Some((start..end, DataKind::VatEu, Validation::Pattern, 0.7));
    }
    None
}

/// Does the national part match what this member state prescribes?
///
/// Lengths and shapes are per the VIES specification. Two deliberate departures from it, both to
/// keep a pattern-only detector from firing on ordinary text:
///
/// - **Romania is published as two to ten digits; here it is six to ten.** `RO12` is
///   indistinguishable from a room number or a product code, and a detector that fires on it will
///   be turned off before it ever catches a tax number.
/// - **Uppercase only.** VAT numbers are written in capitals on every invoice, and accepting
///   lowercase would offer every two-letter word followed by digits to this function.
fn vat_national_part_is_valid(country: &[u8; 2], part: &[u8]) -> bool {
    let len = part.len();
    let digits = |slice: &[u8]| !slice.is_empty() && slice.iter().all(u8::is_ascii_digit);
    let alnum = |slice: &[u8]| {
        !slice.is_empty()
            && slice
                .iter()
                .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
    };

    match country {
        b"AT" => len == 9 && part[0] == b'U' && digits(&part[1..]),
        // A Belgian number is nine digits zero-padded to ten, so it opens with 0 or 1.
        b"BE" => len == 10 && digits(part) && matches!(part[0], b'0' | b'1'),
        b"BG" => matches!(len, 9 | 10) && digits(part),
        b"CY" => len == 9 && digits(&part[..8]) && part[8].is_ascii_uppercase(),
        b"CZ" => matches!(len, 8..=10) && digits(part),
        // Greece is EL in VAT numbers and GR in ISO 3166; both are seen in the wild.
        b"DE" | b"EE" | b"EL" | b"GR" | b"PT" => len == 9 && digits(part),
        b"DK" | b"FI" | b"HU" | b"LU" | b"MT" | b"SI" => len == 8 && digits(part),
        // Spain carries a letter at one end or the other, and sometimes at both.
        b"ES" => {
            len == 9
                && alnum(part)
                && (part[0].is_ascii_uppercase() || part[8].is_ascii_uppercase())
        }
        b"FR" => len == 11 && alnum(&part[..2]) && digits(&part[2..]),
        b"HR" | b"IT" | b"LV" => len == 11 && digits(part),
        b"IE" => matches!(len, 8 | 9) && part[0].is_ascii_digit() && alnum(part),
        b"LT" => matches!(len, 9 | 12) && digits(part),
        // Nine digits, a literal B, then a two-digit branch number.
        b"NL" => len == 12 && digits(&part[..9]) && part[9] == b'B' && digits(&part[10..]),
        b"RO" => (6..=10).contains(&len) && digits(part),
        b"SE" => len == 12 && digits(part),
        b"SK" => len == 10 && digits(part),
        // Northern Ireland keeps a VAT prefix of its own under the Windsor Framework.
        b"XI" => matches!(len, 9 | 12) && digits(part),
        _ => false,
    }
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
