// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Reading text out of attachments, so an identifier inside a document is not invisible.
//!
//! A gateway that inspects prompts and ignores attachments protects the careful user and misses
//! the ordinary one. Measured on 2026-09-01: a PDF carrying a checksum-valid PESEL and IBAN went
//! through the gateway as `findings=clean`, because base64 hides the digits entirely - the bytes
//! `87031406724` are in the file and absent from its encoding, so nothing a scanner does to the
//! request body can find them.
//!
//! What this crate does **not** do is as important as what it does:
//!
//! - **No OCR.** A scanned page is an image, and an image is not read. This is stated rather than
//!   implied, because "we inspect attachments" would otherwise be heard as a promise this cannot
//!   keep.
//! - **No rendering, no scripts, no external references.** Extraction reads bytes and returns
//!   text. A PDF that fetches something, or a document with a macro, is inert here.
//! - **No unbounded work.** Everything is capped by [`Limits`], because this runs in the path of a
//!   request that somebody is waiting for, and a decompression bomb is a denial of service dressed
//!   as a document.

#![warn(missing_docs)]

mod ooxml;
mod pdf;

use base64::Engine as _;

/// Ceilings on the work extraction may do.
///
/// Both matter and for different reasons: the input limit stops a large upload from occupying the
/// request path, and the text limit stops a small archive that expands enormously - a zip bomb is
/// tiny until it is not.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Largest decoded attachment to look at, in bytes. Larger ones are reported, not read.
    pub max_input_bytes: usize,
    /// Largest amount of text to return. Extraction stops here and marks the result truncated.
    pub max_text_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // 10 MB covers the documents people paste into a chat. A 200 MB scan is a different
            // conversation and belongs to policy, not to a parser in the request path.
            max_input_bytes: 10 * 1024 * 1024,
            // 1 MB of text is far more than any prompt, and enough that truncation never hides an
            // identifier that a human would have read.
            max_text_bytes: 1024 * 1024,
        }
    }
}

/// What an attachment turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// PDF, read through its content streams.
    Pdf,
    /// An Office Open XML document: `.docx`, `.xlsx` or `.pptx`.
    Ooxml,
    /// Text that decoded as UTF-8: `.txt`, `.csv`, `.md`, `.json` and anything else plain.
    Text,
    /// Recognised and deliberately not read - an image, an archive, a binary.
    Opaque,
}

impl Kind {
    /// A short, stable name for logs and audit events.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Kind::Pdf => "pdf",
            Kind::Ooxml => "ooxml",
            Kind::Text => "text",
            Kind::Opaque => "opaque",
        }
    }
}

/// Why an attachment produced no text.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// Larger than [`Limits::max_input_bytes`]. Reported so policy can act on it: an attachment
    /// nobody read is not an attachment nobody should worry about.
    #[error("attachment is {size} bytes, over the {limit} byte limit")]
    TooLarge {
        /// Decoded size.
        size: usize,
        /// The configured ceiling.
        limit: usize,
    },
    /// A format this crate does not read: an image, a video, an archive.
    #[error("nothing to read in a {0} attachment")]
    Unreadable(&'static str),
    /// The parser refused. Carries the message, because a malformed document and an encrypted one
    /// need different responses from an operator.
    #[error("could not read the attachment: {0}")]
    Failed(String),
    /// The base64 or data URI did not decode.
    #[error("attachment is not valid base64")]
    NotBase64,
}

/// Text taken out of one attachment.
#[derive(Debug, Clone)]
pub struct Extracted {
    /// What the bytes turned out to be, decided by their content rather than a declared type.
    pub kind: Kind,
    /// The text, ready for the detection layers.
    pub text: String,
    /// True when [`Limits::max_text_bytes`] cut it short. The caller should say so: findings from
    /// a truncated document are a lower bound, not a complete answer.
    pub truncated: bool,
    /// Decoded size of the attachment, for the audit trail.
    pub input_bytes: usize,
}

/// Decode a base64 payload, which may be wrapped in a `data:` URI.
///
/// Both spellings occur in the wild: Anthropic puts bare base64 in `source.data`, OpenAI-compatible
/// clients put a full `data:application/pdf;base64,...` in `file.file_data` or `image_url.url`.
///
/// # Errors
/// [`ExtractError::NotBase64`] when the payload does not decode.
pub fn decode(payload: &str) -> Result<Vec<u8>, ExtractError> {
    let encoded = match payload.strip_prefix("data:") {
        Some(rest) => rest.split_once(";base64,").map_or(rest, |(_, data)| data),
        None => payload,
    };
    // Whitespace is legal in base64 transported through JSON and some clients wrap long lines.
    let cleaned: String = encoded
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(cleaned.as_bytes())
        .map_err(|_| ExtractError::NotBase64)
}

/// Decide what a document is from its first bytes rather than from what the caller called it.
///
/// A declared media type is a claim by the sender, and the sender is the party whose data we are
/// trying not to leak. Magic bytes are a fact.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Kind {
    if bytes.starts_with(b"%PDF-") {
        return Kind::Pdf;
    }
    if bytes.starts_with(b"PK\x03\x04") {
        // Every zip looks alike from here; only the entry names say whether it is a document.
        return if ooxml::looks_like_ooxml(bytes) {
            Kind::Ooxml
        } else {
            Kind::Opaque
        };
    }
    // Images and the other formats worth naming, so the error can say what was skipped.
    const BINARY_MAGIC: [&[u8]; 6] = [
        b"\x89PNG",
        b"\xff\xd8\xff", // JPEG
        b"GIF8",
        b"RIFF",     // WebP and friends
        b"\x1f\x8b", // gzip
        b"%!PS",     // PostScript
    ];
    if BINARY_MAGIC.iter().any(|magic| bytes.starts_with(magic)) {
        return Kind::Opaque;
    }
    if looks_textual(bytes) {
        Kind::Text
    } else {
        Kind::Opaque
    }
}

/// Is this text, in some encoding, rather than a binary?
///
/// Valid UTF-8 is the easy case. The two that matter beyond it were both found by testing rather
/// than by reasoning, and both are ordinary office output:
///
/// - **UTF-16 with a byte order mark.** PowerShell's own editor saves `.ps1` this way, and a
///   UTF-16 file is not valid UTF-8, so it was being skipped as though it were an image.
/// - **A single-byte code page.** A CSV exported from a Polish Excel is Windows-1250, and one
///   accented letter is enough to fail UTF-8 validation and lose the whole file.
///
/// The heuristic for the second case is deliberately coarse: no NUL bytes, and almost everything
/// printable. Identifiers are ASCII in every one of these encodings, so a PESEL survives a wrong
/// guess about which code page it was; a name with diacritics may not, which costs layer 2 and not
/// layer 1. That trade is stated in the documentation rather than left to be discovered.
fn looks_textual(bytes: &[u8]) -> bool {
    // UTF-16 first: it is the one text encoding that legitimately contains NUL bytes, and the NUL
    // test below would otherwise reject every second character of it.
    if utf16_bom(bytes).is_some() {
        return true;
    }
    if bytes.is_empty() || bytes.contains(&0) {
        // A NUL rules it out even when the bytes happen to be valid UTF-8, which a binary made of
        // low bytes usually is. Text files do not contain NUL; object files are full of it.
        return false;
    }
    if std::str::from_utf8(bytes).is_ok() {
        return true;
    }
    let printable = bytes
        .iter()
        .filter(|b| matches!(b, 0x09 | 0x0a | 0x0d | 0x20..=0x7e | 0xa0..=0xff))
        .count();
    printable * 10 >= bytes.len() * 9
}

/// The byte order mark, when the bytes carry one: `true` for little-endian.
fn utf16_bom(bytes: &[u8]) -> Option<bool> {
    match bytes {
        [0xff, 0xfe, ..] => Some(true),
        [0xfe, 0xff, ..] => Some(false),
        _ => None,
    }
}

/// Decode text that may not be UTF-8, keeping ASCII intact whatever it is.
fn decode_text(bytes: &[u8]) -> String {
    if let Some(little_endian) = utf16_bom(bytes) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| {
                if little_endian {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    // A UTF-8 byte order mark is legal and would otherwise arrive as a stray character at the
    // start of the first field, which is the sort of thing that breaks a detector on the first
    // line of a CSV and nowhere else.
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Read whatever text an attachment holds.
///
/// # Errors
/// See [`ExtractError`]. Every failure is a result the caller should record rather than ignore: an
/// attachment that could not be read is exactly the one an operator may want to stop.
pub fn extract(bytes: &[u8], limits: &Limits) -> Result<Extracted, ExtractError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(ExtractError::TooLarge {
            size: bytes.len(),
            limit: limits.max_input_bytes,
        });
    }

    let kind = sniff(bytes);
    let text = match kind {
        Kind::Pdf => pdf::text(bytes, limits)?,
        Kind::Ooxml => ooxml::text(bytes, limits)?,
        Kind::Text => decode_text(bytes),
        Kind::Opaque => return Err(ExtractError::Unreadable("binary")),
    };

    let truncated = text.len() > limits.max_text_bytes;
    let text = if truncated {
        // On a character boundary, or the String is invalid.
        let mut cut = limits.max_text_bytes;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text[..cut].to_string()
    } else {
        text
    };

    Ok(Extracted {
        kind,
        text,
        truncated,
        input_bytes: bytes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_data_uri_and_bare_base64_decode_the_same() {
        let bare = "SGVsbG8=";
        let uri = "data:text/plain;base64,SGVsbG8=";
        assert_eq!(decode(bare).unwrap(), b"Hello");
        assert_eq!(decode(uri).unwrap(), b"Hello");
    }

    #[test]
    fn wrapped_base64_still_decodes() {
        // Some clients wrap at 76 characters, which is legal and would otherwise fail.
        assert_eq!(decode("SGVs\nbG8=").unwrap(), b"Hello");
    }

    #[test]
    fn the_type_comes_from_the_bytes_not_from_the_sender() {
        assert_eq!(sniff(b"%PDF-1.4\n..."), Kind::Pdf);
        assert_eq!(sniff(b"\x89PNG\r\n"), Kind::Opaque);
        assert_eq!(sniff("PESEL 87031406724".as_bytes()), Kind::Text);
        // FF FE is a UTF-16 byte order mark, so those bytes are text now. A compiled object is
        // not: it is full of NUL, whatever else it contains.
        assert_eq!(sniff(&[0xff, 0xfe, 0x00, 0x41]), Kind::Text);
        assert_eq!(sniff(&[0x4d, 0x5a, 0x90, 0x00, 0x03, 0x00]), Kind::Opaque);
    }

    #[test]
    fn text_is_read_as_text() {
        let result = extract(
            b"Klient Marek Nowak, PESEL 87031406724.",
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(result.kind, Kind::Text);
        assert!(result.text.contains("87031406724"));
        assert!(!result.truncated);
    }

    #[test]
    fn an_oversized_attachment_is_reported_rather_than_read() {
        // The size is what policy acts on: something too big to inspect is not something safe.
        let limits = Limits {
            max_input_bytes: 10,
            ..Limits::default()
        };
        let err = extract(b"far more than ten bytes", &limits).unwrap_err();
        assert!(matches!(err, ExtractError::TooLarge { .. }), "{err}");
    }

    #[test]
    fn text_is_truncated_on_a_character_boundary() {
        // Cutting a multi-byte character in half would panic on the slice, and Polish text is
        // full of them.
        let limits = Limits {
            max_text_bytes: 5,
            ..Limits::default()
        };
        let result = extract("zażółć gęślą jaźń".as_bytes(), &limits).unwrap();
        assert!(result.truncated);
        assert!(result.text.len() <= 5);
    }

    #[test]
    fn a_powershell_script_saved_as_utf16_is_read() {
        // PowerShell's own editor saves .ps1 as UTF-16 with a BOM, which is not valid UTF-8 - so
        // this file was being skipped as though it were an image.
        let mut bytes = vec![0xff, 0xfe];
        for unit in "$pesel = \"87031406724\"".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let result = extract(&bytes, &Limits::default()).unwrap();
        assert_eq!(result.kind, Kind::Text);
        assert!(result.text.contains("87031406724"), "{:?}", result.text);
    }

    #[test]
    fn a_csv_in_a_single_byte_code_page_still_yields_its_identifiers() {
        // A CSV exported from a Polish Excel is Windows-1250. One accented letter fails UTF-8
        // validation, and the whole file used to be dropped for it. Identifiers are ASCII in
        // every one of these encodings, so they survive a wrong guess about which one it was.
        let mut bytes = b"imi".to_vec();
        bytes.push(0xea); // 'e with ogonek' in Windows-1250
        bytes.extend_from_slice(
            b";pesel
Marek;87031406724
",
        );
        let result = extract(&bytes, &Limits::default()).unwrap();
        assert_eq!(result.kind, Kind::Text);
        assert!(result.text.contains("87031406724"), "{:?}", result.text);
    }

    #[test]
    fn a_utf8_byte_order_mark_does_not_reach_the_detectors() {
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(b"pesel;87031406724");
        let result = extract(&bytes, &Limits::default()).unwrap();
        assert!(result.text.starts_with("pesel"), "{:?}", result.text);
    }

    #[test]
    fn a_binary_is_still_a_binary() {
        // The textual heuristic must not swallow files it cannot read: a NUL byte and a run of
        // unprintable bytes are what separates a code page from a compiled object.
        let mut bytes = vec![0u8; 64];
        bytes[0] = 0x7f;
        assert_eq!(sniff(&bytes), Kind::Opaque);
    }

    #[test]
    fn an_image_says_what_it_is_instead_of_returning_nothing() {
        let err = extract(b"\x89PNG\r\n\x1a\n", &Limits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::Unreadable(_)), "{err}");
    }
}
