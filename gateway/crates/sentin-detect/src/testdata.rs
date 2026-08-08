// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Generators for **synthetic** identifiers with correct checksums.
//!
//! The project forbids real personal data in fixtures, so tests and benchmarks need a way to
//! produce identifiers that are structurally valid but belong to nobody. Hand-writing them does
//! not work: a number invented by eye fails its own checksum, and a test built on it "fails"
//! while the code under test is right.
//!
//! These are also what the property-based tests use to explore the input space, and what the
//! throughput benchmark uses to build realistic haystacks.

/// Build a PESEL for the given date and serial, appending the correct check digit.
///
/// `year` is a full year; the month is encoded with the century offset PESEL requires.
#[must_use]
pub fn pesel(year: u32, month: u32, day: u32, serial: u32) -> String {
    let encoded_month = month
        + match year {
            1800..=1899 => 80,
            1900..=1999 => 0,
            2000..=2099 => 20,
            2100..=2199 => 40,
            _ => 60,
        };
    let mut digits: Vec<u8> = format!(
        "{:02}{:02}{:02}{:04}",
        year % 100,
        encoded_month,
        day,
        serial % 10_000
    )
    .bytes()
    .map(|b| b - b'0')
    .collect();

    const WEIGHTS: [u32; 10] = [1, 3, 7, 9, 1, 3, 7, 9, 1, 3];
    let sum: u32 = WEIGHTS
        .iter()
        .zip(&digits)
        .map(|(w, d)| w * u32::from(*d))
        .sum();
    digits.push(u8::try_from((10 - sum % 10) % 10).expect("check digit is one digit"));
    to_string(&digits)
}

/// Build a NIP from nine base digits. Returns `None` for the ~9 % of inputs whose remainder is 10,
/// which cannot be expressed as a check digit and are therefore never issued.
#[must_use]
pub fn nip(base: [u8; 9]) -> Option<String> {
    const WEIGHTS: [u32; 9] = [6, 5, 7, 2, 3, 4, 5, 6, 7];
    let sum: u32 = WEIGHTS
        .iter()
        .zip(&base)
        .map(|(w, d)| w * u32::from(*d))
        .sum();
    let check = sum % 11;
    if check == 10 {
        return None;
    }
    let mut digits = base.to_vec();
    digits.push(u8::try_from(check).expect("check digit is one digit"));
    Some(to_string(&digits))
}

/// Build a 9-digit REGON from eight base digits.
#[must_use]
pub fn regon9(base: [u8; 8]) -> String {
    const WEIGHTS: [u32; 8] = [8, 9, 2, 3, 4, 5, 6, 7];
    let sum: u32 = WEIGHTS
        .iter()
        .zip(&base)
        .map(|(w, d)| w * u32::from(*d))
        .sum();
    let mut digits = base.to_vec();
    digits.push(u8::try_from((sum % 11) % 10).expect("check digit is one digit"));
    to_string(&digits)
}

/// Build a 14-digit REGON from thirteen base digits.
#[must_use]
pub fn regon14(base: [u8; 13]) -> String {
    const WEIGHTS: [u32; 13] = [2, 4, 8, 5, 0, 9, 7, 3, 6, 1, 2, 4, 8];
    let sum: u32 = WEIGHTS
        .iter()
        .zip(&base)
        .map(|(w, d)| w * u32::from(*d))
        .sum();
    let mut digits = base.to_vec();
    digits.push(u8::try_from((sum % 11) % 10).expect("check digit is one digit"));
    to_string(&digits)
}

/// Build a Luhn-valid card number of `len` digits starting with `prefix`.
///
/// The prefix decides which issuer the detector will see, so callers choose it to exercise a
/// specific network.
#[must_use]
pub fn card(prefix: &str, len: usize) -> String {
    let mut digits: Vec<u8> = prefix.bytes().map(|b| b - b'0').collect();
    // Deterministic filler: reproducible failures matter more than variety here.
    let mut seed = 7u8;
    while digits.len() < len - 1 {
        seed = (seed * 3 + 1) % 10;
        digits.push(seed);
    }

    let mut sum = 0u32;
    for (index, digit) in digits.iter().rev().enumerate() {
        let mut value = u32::from(*digit);
        if index % 2 == 0 {
            value *= 2;
            if value > 9 {
                value -= 9;
            }
        }
        sum += value;
    }
    digits.push(u8::try_from((10 - sum % 10) % 10).expect("check digit is one digit"));
    to_string(&digits)
}

/// Build an IBAN for `country` from a BBAN, computing the ISO 13616 check digits.
#[must_use]
pub fn iban(country: &str, bban: &str) -> String {
    let mut remainder = 0u32;
    let feed = |remainder: &mut u32, byte: u8| match byte {
        b'0'..=b'9' => *remainder = (*remainder * 10 + u32::from(byte - b'0')) % 97,
        b'A'..=b'Z' => *remainder = (*remainder * 100 + u32::from(byte - b'A') + 10) % 97,
        _ => {}
    };

    for byte in bban.bytes().chain(country.bytes()) {
        feed(&mut remainder, byte.to_ascii_uppercase());
    }
    // Two placeholder zeros stand in for the check digits during the computation.
    remainder = (remainder * 100) % 97;
    let check = 98 - remainder;
    format!("{}{:02}{}", country.to_ascii_uppercase(), check, bban)
}

fn to_string(digits: &[u8]) -> String {
    digits.iter().map(|d| char::from(b'0' + d)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checksums;

    fn digits(s: &str) -> Vec<u8> {
        s.bytes()
            .filter(u8::is_ascii_digit)
            .map(|b| b - b'0')
            .collect()
    }

    #[test]
    fn generated_identifiers_satisfy_their_own_checksums() {
        assert!(checksums::pesel(&digits(&super::pesel(1944, 5, 14, 135))));
        assert!(checksums::pesel(&digits(&super::pesel(2002, 5, 5, 1446))));

        let generated_nip = super::nip([1, 2, 3, 4, 5, 6, 3, 2, 1]).expect("remainder is not 10");
        assert!(checksums::nip(&digits(&generated_nip)));

        assert!(checksums::regon(&digits(&regon9([1, 2, 3, 4, 5, 6, 7, 8]))));
        assert!(checksums::regon(&digits(&regon14([
            1, 2, 3, 4, 5, 6, 7, 8, 5, 1, 2, 3, 4
        ]))));

        for (prefix, len) in [("4", 16), ("4", 13), ("51", 16), ("34", 15), ("6011", 16)] {
            let number = card(prefix, len);
            assert_eq!(number.len(), len);
            assert!(checksums::luhn(&digits(&number)), "{number} fails Luhn");
        }

        let generated_iban = super::iban("PL", "109010140000071219812874");
        assert!(
            checksums::iban(generated_iban.as_bytes()),
            "{generated_iban}"
        );
    }

    #[test]
    fn generated_pesel_encodes_the_century_in_the_month() {
        assert!(pesel(1944, 5, 14, 135).starts_with("4405"));
        assert!(pesel(2002, 5, 5, 1446).starts_with("0225"));
        assert!(pesel(1885, 12, 1, 1).starts_with("8592"));
    }
}
