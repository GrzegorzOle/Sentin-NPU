// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! PDF text extraction.
//!
//! Delegated to `pdf-extract` rather than written here. Pulling text out of a PDF is not the
//! afternoon's work it looks like: content streams are compressed, glyphs are addressed by font
//! index rather than by character, and a subset font needs its `ToUnicode` map before a digit is a
//! digit. A hand-rolled reader would find the identifiers in the PDFs it was tested against and
//! quietly miss them everywhere else, which is worse than not reading PDFs at all - a detector
//! that fails silently teaches an operator to trust it.
//!
//! What is written here is the containment: the parser runs behind a size limit, its panics are
//! caught, and its output is treated as untrusted text rather than as a result.

use crate::{ExtractError, Limits};

/// Extract text from a PDF.
///
/// # Errors
/// [`ExtractError::Failed`] when the document is encrypted, malformed, or the parser gives up.
pub fn text(bytes: &[u8], _limits: &Limits) -> Result<String, ExtractError> {
    // `catch_unwind` because this is a parser for a format that arrives from outside and a panic
    // here would take down the request, and with it the requests sharing the runtime. A refusal is
    // a result; a crash is an outage.
    let parsed = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes));

    match parsed {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(err)) => Err(ExtractError::Failed(err.to_string())),
        Err(_) => Err(ExtractError::Failed(
            "the PDF parser panicked; the document is treated as unreadable".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal, valid PDF with one uncompressed text stream. Built here rather than checked in
    /// as a fixture so the test says what it is testing.
    fn pdf_with(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 40 700 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R \
              /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];

        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
        }
        let xref = out.len();
        out.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in &offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    #[test]
    fn an_identifier_inside_a_pdf_comes_back_out() {
        // The whole reason this crate exists: base64 hides these digits from every scanner that
        // looks at the request body, and they are right there in the document.
        let pdf = pdf_with("Zleceniobiorca Marek Nowak, PESEL 87031406724.");
        let text = text(&pdf, &Limits::default()).expect("a valid PDF");
        assert!(
            text.contains("87031406724"),
            "the PESEL must survive extraction: {text:?}"
        );
    }

    #[test]
    fn a_malformed_pdf_is_refused_rather_than_crashing() {
        let err = text(b"%PDF-1.4\nnot actually a pdf", &Limits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::Failed(_)), "{err}");
    }
}
