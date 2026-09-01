// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Office Open XML: `.docx`, `.xlsx`, `.pptx`.
//!
//! These are zip archives of XML, which makes them the easy case - and the one with the sharpest
//! edge. A zip is an instruction to decompress, and an attachment is written by whoever sent it,
//! so every entry is read through a limited reader and the total is capped. A 40 KB archive that
//! expands to a gigabyte is a denial of service wearing a document's clothes.
//!
//! Text is taken from the parts that hold prose and the parts that hold data:
//!
//! - `word/document.xml` and the headers and footers beside it, because a letterhead carries names,
//! - `xl/sharedStrings.xml`, where a spreadsheet keeps every string it displays,
//! - `ppt/slides/*.xml`.
//!
//! Comments, tracked changes and speaker notes are read too. An identifier deleted in a tracked
//! change is still in the file, and still leaves the machine.

use std::io::Read;

use quick_xml::events::Event;

use crate::{ExtractError, Limits};

/// Entries whose text is worth reading, matched as prefixes.
const WANTED: [&str; 8] = [
    "word/document.xml",
    "word/header",
    "word/footer",
    "word/comments.xml",
    "xl/sharedStrings.xml",
    // Where a spreadsheet keeps its *numbers*. sharedStrings holds only the strings, so a PESEL
    // typed into a cell as a number - which is what happens when you type one into Excel - lived
    // here and nowhere else, and was invisible until this line existed.
    "xl/worksheets/sheet",
    "ppt/slides/slide",
    // OpenDocument, which is the same idea in a different package.
    "content.xml",
];

/// Does this zip look like an Office document rather than any other archive?
pub fn looks_like_ooxml(bytes: &[u8]) -> bool {
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return false;
    };
    // `[Content_Types].xml` is mandatory in an OOXML package; OpenDocument uses a `mimetype`
    // entry instead. Bound to a name rather than returned directly: the entry borrows the archive.
    let found =
        archive.by_name("[Content_Types].xml").is_ok() || archive.by_name("mimetype").is_ok();
    found
}

/// Extract the readable text from an OOXML package.
///
/// # Errors
/// [`ExtractError::Failed`] when the archive will not open, and
/// [`ExtractError::Unreadable`] when it opens but holds none of the parts that carry text.
pub fn text(bytes: &[u8], limits: &Limits) -> Result<String, ExtractError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|err| ExtractError::Failed(err.to_string()))?;

    // Names first, in their own scope: an entry borrows the archive, and the loop below needs the
    // archive back to open them one at a time.
    let mut names: Vec<String> = Vec::new();
    for index in 0..archive.len() {
        if let Ok(entry) = archive.by_index(index) {
            let name = entry.name().to_string();
            if WANTED.iter().any(|wanted| name.starts_with(wanted)) {
                names.push(name);
            }
        }
    }

    if names.is_empty() {
        return Err(ExtractError::Unreadable(
            "office document with no text parts",
        ));
    }

    let mut out = String::new();
    for name in names {
        if out.len() >= limits.max_text_bytes {
            break;
        }
        let Ok(entry) = archive.by_name(&name) else {
            continue;
        };
        // The remaining budget, not the entry's declared size: a declared size is a claim by the
        // archive, and this is exactly where a zip bomb makes its claim.
        let budget = limits.max_text_bytes.saturating_sub(out.len());
        let mut xml = String::new();
        if entry.take(budget as u64).read_to_string(&mut xml).is_err() {
            // Not UTF-8, or truncated mid-character. Skip the part rather than the document.
            continue;
        }
        push_text(&xml, &mut out);
    }

    if out.trim().is_empty() {
        return Err(ExtractError::Unreadable(
            "office document with no readable text",
        ));
    }
    Ok(out)
}

/// Append the character data of an XML document, one space between elements.
///
/// Word splits a single word across several runs whenever formatting changes, so joining without a
/// separator would glue neighbouring words together and joining with one breaks a number in half.
/// Paragraph and cell boundaries are what deserve a space; runs inside a paragraph are not.
fn push_text(xml: &str, out: &mut String) {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Text(text)) => {
                // Already decoded and unescaped by this version of the reader: `&amp;` arrives as
                // an ampersand, so a name written as `Ma&#114;ek` reaches the detectors whole
                // rather than as three tokens it would never match.
                out.push_str(text.as_ref());
            }
            // A paragraph, a table cell, a line break or a slide ends a piece of text. Without
            // this, "Marek" and "Nowak" from two paragraphs would become "MarekNowak".
            Ok(Event::End(tag)) => {
                let name = tag.name();
                let local = name.local_name();
                // `v` is a spreadsheet cell value and `text-p` an OpenDocument paragraph.
                if matches!(
                    local.as_ref(),
                    "p" | "tc" | "tr" | "br" | "si" | "t" | "v" | "c" | "text-p"
                ) {
                    out.push(' ');
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn docx_with(paragraphs: &[&str]) -> Vec<u8> {
        let body: String = paragraphs
            .iter()
            .map(|p| format!("<w:p><w:r><w:t>{p}</w:t></w:r></w:p>"))
            .collect();
        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>{body}</w:body></w:document>"#
        );

        let mut buffer = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("[Content_Types].xml", options).unwrap();
            zip.write_all(b"<Types/>").unwrap();
            zip.start_file("word/document.xml", options).unwrap();
            zip.write_all(document.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buffer
    }

    #[test]
    fn a_docx_is_recognised_and_an_ordinary_zip_is_not() {
        assert!(looks_like_ooxml(&docx_with(&["hello"])));

        let mut plain = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut plain));
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("notes.txt", options).unwrap();
            zip.write_all(b"nothing to see").unwrap();
            zip.finish().unwrap();
        }
        assert!(!looks_like_ooxml(&plain), "a plain zip is not a document");
    }

    #[test]
    fn identifiers_survive_a_docx() {
        let docx = docx_with(&["Umowa zlecenia", "Marek Nowak, PESEL 87031406724"]);
        let text = text(&docx, &Limits::default()).unwrap();
        assert!(text.contains("87031406724"), "{text:?}");
        assert!(text.contains("Marek Nowak"), "{text:?}");
    }

    #[test]
    fn paragraphs_do_not_run_into_each_other() {
        // Without a separator at the end of a paragraph these two words would arrive as one, and
        // the NER model would see a name that does not exist.
        let docx = docx_with(&["Marek", "Nowak"]);
        let text = text(&docx, &Limits::default()).unwrap();
        assert!(!text.contains("MarekNowak"), "{text:?}");
    }

    #[test]
    fn extraction_stops_at_the_text_limit() {
        let long = "PESEL 87031406724 ".repeat(5000);
        let docx = docx_with(&[&long]);
        let limits = Limits {
            max_text_bytes: 512,
            ..Limits::default()
        };
        let text = text(&docx, &limits).unwrap();
        assert!(
            text.len() <= 512 + 64,
            "the budget must bound the work: {}",
            text.len()
        );
    }

    #[test]
    fn a_number_typed_into_a_spreadsheet_cell_is_found() {
        // sharedStrings holds only strings. Type a PESEL into Excel and it is a number, living in
        // the worksheet and nowhere else - which is exactly where identifiers end up in practice.
        let mut buffer = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("[Content_Types].xml", options).unwrap();
            zip.write_all(b"<Types/>").unwrap();
            zip.start_file("xl/sharedStrings.xml", options).unwrap();
            zip.write_all(b"<sst><si><t>Marek Nowak</t></si></sst>")
                .unwrap();
            zip.start_file("xl/worksheets/sheet1.xml", options).unwrap();
            zip.write_all(
                b"<worksheet><sheetData><row><c t=\"s\"><v>0</v></c>                  <c><v>87031406724</v></c></row></sheetData></worksheet>",
            )
            .unwrap();
            zip.finish().unwrap();
        }
        let text = text(&buffer, &Limits::default()).unwrap();
        assert!(text.contains("87031406724"), "{text:?}");
        assert!(text.contains("Marek Nowak"), "{text:?}");
    }

    #[test]
    fn an_office_file_with_nothing_to_read_says_so() {
        let mut buffer = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("[Content_Types].xml", options).unwrap();
            zip.write_all(b"<Types/>").unwrap();
            zip.finish().unwrap();
        }
        let err = text(&buffer, &Limits::default()).unwrap_err();
        assert!(matches!(err, ExtractError::Unreadable(_)), "{err}");
    }
}
