// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Arithmetic validation for the structured identifiers layer 1 recognises.
//!
//! Every function here takes digits already extracted from the text (values `0..=9`, separators
//! stripped) and answers one question: does this pass its checksum? Nothing here looks at the
//! surrounding text — that is the scanner's job.
//!
//! These checks are what earns a finding [`sentin_core::Validation::Checksum`], and therefore the
//! right to block a request. They must have no false *positives* by construction: a wrong answer
//! here becomes a refused user request.

/// PESEL — Polish national identification number.
///
/// Eleven digits: a birth date, a serial with sex in the last digit, and a check digit. Both the
/// checksum *and* the date are validated: the checksum alone passes roughly one random 11-digit
/// string in ten, and a DLP tool that fires that often gets turned off.
#[must_use]
pub fn pesel(digits: &[u8]) -> bool {
    const WEIGHTS: [u32; 10] = [1, 3, 7, 9, 1, 3, 7, 9, 1, 3];
    if digits.len() != 11 {
        return false;
    }

    let sum: u32 = WEIGHTS
        .iter()
        .zip(&digits[..10])
        .map(|(w, d)| w * u32::from(*d))
        .sum();
    if (10 - sum % 10) % 10 != u32::from(digits[10]) {
        return false;
    }

    pesel_date_is_valid(digits)
}

/// PESEL encodes the century by offsetting the month, which is why this is not a plain date parse.
fn pesel_date_is_valid(digits: &[u8]) -> bool {
    let two = |i: usize| u32::from(digits[i]) * 10 + u32::from(digits[i + 1]);
    let (year_in_century, encoded_month, day) = (two(0), two(2), two(4));

    // 01-12 => 1900s, 21-32 => 2000s, 41-52 => 2100s, 61-72 => 2200s, 81-92 => 1800s.
    let (century, month) = match encoded_month {
        1..=12 => (1900, encoded_month),
        21..=32 => (2000, encoded_month - 20),
        41..=52 => (2100, encoded_month - 40),
        61..=72 => (2200, encoded_month - 60),
        81..=92 => (1800, encoded_month - 80),
        _ => return false,
    };

    day >= 1 && day <= days_in_month(century + year_in_century, month)
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// NIP — Polish tax identification number. Ten digits, weighted mod 11.
#[must_use]
pub fn nip(digits: &[u8]) -> bool {
    const WEIGHTS: [u32; 9] = [6, 5, 7, 2, 3, 4, 5, 6, 7];
    if digits.len() != 10 {
        return false;
    }
    let sum: u32 = WEIGHTS
        .iter()
        .zip(&digits[..9])
        .map(|(w, d)| w * u32::from(*d))
        .sum();
    let check = sum % 11;
    // A remainder of 10 cannot be represented in one digit, so such numbers are simply not issued.
    check != 10 && check == u32::from(digits[9])
}

/// REGON — Polish business registry number, in its 9-digit and 14-digit forms.
#[must_use]
pub fn regon(digits: &[u8]) -> bool {
    const WEIGHTS_9: [u32; 8] = [8, 9, 2, 3, 4, 5, 6, 7];
    const WEIGHTS_14: [u32; 13] = [2, 4, 8, 5, 0, 9, 7, 3, 6, 1, 2, 4, 8];

    let weights: &[u32] = match digits.len() {
        9 => &WEIGHTS_9,
        14 => &WEIGHTS_14,
        _ => return false,
    };
    let sum: u32 = weights
        .iter()
        .zip(digits)
        .map(|(w, d)| w * u32::from(*d))
        .sum();
    // Unlike NIP, a remainder of 10 folds to 0 rather than invalidating the number.
    (sum % 11) % 10 == u32::from(digits[digits.len() - 1])
}

/// Luhn check, used by payment cards.
///
/// On its own this is weak — about one random digit string in ten passes — so the card detector
/// also requires a recognised issuer prefix. See `deterministic::card_prefix_is_known`.
#[must_use]
pub fn luhn(digits: &[u8]) -> bool {
    if digits.len() < 12 {
        return false;
    }
    let mut sum = 0u32;
    for (index, digit) in digits.iter().rev().enumerate() {
        let mut value = u32::from(*digit);
        if index % 2 == 1 {
            value *= 2;
            if value > 9 {
                value -= 9;
            }
        }
        sum += value;
    }
    sum % 10 == 0
}

/// IBAN mod-97 check (ISO 13616), over the alphanumeric characters of the account number.
///
/// Input is the uppercased IBAN with separators removed. The remainder is computed incrementally
/// so no big-integer arithmetic is needed for the up-to-34-character value.
#[must_use]
pub fn iban(chars: &[u8]) -> bool {
    if !(15..=34).contains(&chars.len()) {
        return false;
    }
    if !chars[..2].iter().all(u8::is_ascii_uppercase) || !chars[2..4].iter().all(u8::is_ascii_digit)
    {
        return false;
    }

    // The first four characters move to the end before the check is computed.
    let rearranged = chars[4..].iter().chain(&chars[..4]);
    let mut remainder: u32 = 0;
    for byte in rearranged {
        remainder = match byte {
            b'0'..=b'9' => remainder * 10 + u32::from(byte - b'0'),
            b'A'..=b'Z' => {
                // Letters expand to two digits (A=10 .. Z=35), so the shift is by 100.
                let value = u32::from(byte - b'A') + 10;
                remainder * 100 + value
            }
            _ => return false,
        } % 97;
    }
    remainder == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digits(s: &str) -> Vec<u8> {
        s.bytes()
            .filter(u8::is_ascii_digit)
            .map(|b| b - b'0')
            .collect()
    }

    #[test]
    fn pesel_accepts_valid_synthetic_numbers() {
        // Generated, never transcribed: an identifier invented by eye fails its own checksum.
        for value in [
            crate::testdata::pesel(1944, 5, 14, 135),
            crate::testdata::pesel(2002, 7, 8, 362),
            crate::testdata::pesel(1985, 1, 1, 1234),
        ] {
            assert!(pesel(&digits(&value)), "{value} should be a valid PESEL");
        }
    }

    #[test]
    fn pesel_rejects_bad_checksum_and_impossible_dates() {
        assert!(!pesel(&digits("44051401358")), "checksum off by one");
        assert!(!pesel(&digits("44133101234")), "month 13 does not exist");
        assert!(!pesel(&digits("44023001234")), "30 February does not exist");
        assert!(!pesel(&digits("4405140135")), "too short");
    }

    #[test]
    fn pesel_handles_century_encoded_months() {
        // Month 25 encodes May 2002; the same date in 1902 would be written with month 05.
        let born_2002 = crate::testdata::pesel(2002, 5, 5, 1446);
        assert_eq!(&born_2002[2..4], "25");
        assert!(pesel(&digits(&born_2002)));
    }

    #[test]
    fn pesel_february_29_follows_leap_rules() {
        // Month 02 means the 1900s, and 1900 is *not* a leap year -- divisible by 100, not 400.
        assert!(
            !pesel_date_is_valid(&digits("00022900000")),
            "1900 not leap"
        );
        assert!(
            !pesel_date_is_valid(&digits("01022900000")),
            "1901 not leap"
        );
        // Month 22 shifts the same date into 2000, which is a leap year.
        assert!(pesel_date_is_valid(&digits("00222900000")), "2000 is leap");
        assert!(pesel_date_is_valid(&digits("04022900000")), "1904 is leap");
    }

    #[test]
    fn nip_validates_weighted_mod_11() {
        assert!(nip(&digits("1234563218")));
        assert!(!nip(&digits("1234563219")));
        assert!(!nip(&digits("123456321")), "too short");
    }

    #[test]
    fn regon_validates_both_lengths() {
        assert!(regon(&digits("123456785")));
        assert!(!regon(&digits("123456786")));
        assert!(regon(&digits("12345678512347")));
        assert!(!regon(&digits("1234567851234")), "13 digits is not a REGON");
    }

    #[test]
    fn luhn_matches_known_test_numbers() {
        // Card-industry test numbers, not real accounts.
        assert!(luhn(&digits("4111111111111111")));
        assert!(luhn(&digits("5500005555555559")));
        assert!(!luhn(&digits("4111111111111112")));
    }

    #[test]
    fn iban_validates_mod_97() {
        assert!(iban(b"PL61109010140000071219812874"));
        assert!(iban(b"GB82WEST12345698765432"));
        assert!(!iban(b"PL61109010140000071219812875"));
        assert!(!iban(b"PL6110901014"), "too short");
    }
}
