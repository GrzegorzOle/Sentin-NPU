// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Editing an installed `config.yaml` in place, without rewriting it.
//!
//! The obvious implementation - deserialise into [`Config`], change a field, serialise back - is
//! wrong here, and visibly so the first time somebody opens the result. The installed file is
//! written by the installer with comments explaining why `model_dir` must be absolute and why the
//! port is 4141 and not 4000; a round trip through serde discards every one of them, reorders the
//! map keys, and hands the operator back a file that no longer explains itself. It would also
//! rewrite fields nobody touched, so a defect in this crate could change the bind address of a
//! gateway somebody only wanted to add a detector to.
//!
//! So this module edits **lines**. It locates a key by walking indentation, replaces the smallest
//! span that carries the value, and leaves every other byte of the file alone - comments, blank
//! lines, key order, alignment, and the line endings, which on an installed Windows configuration
//! are CRLF.
//!
//! Two safety properties hold before anything reaches disk:
//!
//! 1. **The result must parse as [`Config`]** - the gateway's own type, not a lookalike. An edit
//!    that produces a file the gateway would refuse to start on is rejected while the operator is
//!    still looking at the screen, rather than at the next service restart.
//! 2. **The reparsed value must be the one that was asked for.** Parsing proves the file is valid
//!    YAML; it does not prove the edit landed on the right line. A file with two `path:` keys
//!    parses perfectly and means something else.

use std::path::{Path, PathBuf};

use sentin_core::{DataKind, Decision};
use sentin_proxy::config::{detector_key, Config};

/// What can go wrong between opening a configuration and writing it back.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// The file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file being read or written.
        path: PathBuf,
        /// The underlying failure.
        source: std::io::Error,
    },
    /// The file does not parse as a gateway configuration.
    #[error("this file is not a gateway configuration: {0}")]
    Parse(String),
    /// The file parses, but does not have the shape the edit needs.
    #[error("{0}")]
    Shape(String),
    /// The edit was applied and the result does not say what it was meant to say.
    ///
    /// This is the interesting one: it means a key was found in the wrong place, and it is caught
    /// before the file is written rather than after the service restarts on it.
    #[error("the edit did not take effect as written ({0}); nothing was saved")]
    NotApplied(String),
}

/// An installed configuration, held as text and edited as text.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    path: PathBuf,
    lines: Vec<String>,
}

impl ConfigFile {
    /// Read a configuration from disk.
    ///
    /// # Errors
    /// If the file cannot be read, or does not parse as a gateway configuration. Refusing to open
    /// a file this crate cannot parse is deliberate: an editor that opens anything will eventually
    /// save over something that was not a configuration at all.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PolicyError> {
        let path = path.as_ref().to_path_buf();
        let text = std::fs::read_to_string(&path).map_err(|source| PolicyError::Io {
            path: path.clone(),
            source,
        })?;
        Self::from_text(path, &text)
    }

    /// Build from text that is already in hand, which is what the tests use.
    ///
    /// # Errors
    /// If the text does not parse as a gateway configuration.
    pub fn from_text(path: PathBuf, text: &str) -> Result<Self, PolicyError> {
        serde_yaml_ng::from_str::<Config>(text).map_err(|e| PolicyError::Parse(e.to_string()))?;
        Ok(Self {
            path,
            // `split('\n')` rather than `lines()`: `lines()` discards the carriage return of a CRLF
            // file, so writing back would silently convert an installed Windows configuration to
            // LF endings. Keeping `\r` as the last byte of each line and joining on `\n` round
            // trips any file exactly.
            lines: text.split('\n').map(ToString::to_string).collect(),
        })
    }

    /// Where this configuration lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The current text, exactly as it would be written.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Parse the current text with the gateway's own configuration type.
    ///
    /// # Errors
    /// If the text no longer parses, which for an edit made by this module is a bug.
    pub fn parsed(&self) -> Result<Config, PolicyError> {
        serde_yaml_ng::from_str(&self.text()).map_err(|e| PolicyError::Parse(e.to_string()))
    }

    /// The mode configured for a detector, or `None` when the file does not mention it.
    ///
    /// `None` is not "off". An unconfigured detector falls back to observing, so the identifier is
    /// found, recorded in the audit trail, and forwarded - which is why anything showing this to an
    /// operator must show the fallback rather than a blank.
    #[must_use]
    pub fn detector_mode(&self, kind: DataKind) -> Option<Decision> {
        let key = detector_key(kind);
        let index = self.detector_line(key)?;
        parse_mode(&self.lines[index])
    }

    /// Set a detector's mode, adding the entry when the file does not carry it.
    ///
    /// # Errors
    /// If the requested mode is stronger than the detector's evidence can justify, if the
    /// `detectors:` block cannot be found, or if the result does not reparse to the requested
    /// value.
    pub fn set_detector_mode(&mut self, kind: DataKind, mode: Decision) -> Result<(), PolicyError> {
        // Checked here and not only in the interface, because this is the layer that writes the
        // file. `mode: block` on an email address parses and is then clamped at every request, so
        // accepting it would store a policy that the gateway does not run - and the operator would
        // have no way to tell from the file.
        if mode > kind.max_decision() {
            return Err(PolicyError::Shape(format!(
                "{} cannot {}: {}",
                detector_key(kind),
                mode_word(mode),
                why_not(kind)
            )));
        }

        let key = detector_key(kind);
        match self.detector_line(key) {
            Some(index) => {
                let edited = replace_mode(&self.lines[index], mode).ok_or_else(|| {
                    PolicyError::Shape(format!(
                        "the entry for {key} is not of the form `{{ layer: ..., mode: ... }}`"
                    ))
                })?;
                self.lines[index] = edited;
            }
            None => self.insert_detector(key, mode)?,
        }

        let got = self.parsed()?.detectors.get(key).map(|rule| rule.mode);
        if got != Some(mode) {
            return Err(PolicyError::NotApplied(format!(
                "{key} reads back as {got:?}, not {mode:?}"
            )));
        }
        Ok(())
    }

    /// Whether the JSONL audit sink is on, and where it writes.
    ///
    /// # Errors
    /// If the file does not parse.
    pub fn audit_jsonl(&self) -> Result<(bool, String), PolicyError> {
        let config = self.parsed()?;
        Ok((config.audit.jsonl.enabled, config.audit.jsonl.path))
    }

    /// Turn the JSONL audit sink on or off.
    ///
    /// # Errors
    /// If the key cannot be located, or the result does not reparse to the requested value.
    pub fn set_audit_enabled(&mut self, enabled: bool) -> Result<(), PolicyError> {
        self.set_scalar(&["audit", "jsonl", "enabled"], &enabled.to_string())?;
        if self.parsed()?.audit.jsonl.enabled != enabled {
            return Err(PolicyError::NotApplied("audit.jsonl.enabled".into()));
        }
        Ok(())
    }

    /// Point the JSONL audit sink at a different file.
    ///
    /// # Errors
    /// If the path is relative, if the key cannot be located, or if the result does not reparse.
    pub fn set_audit_path(&mut self, path: &str) -> Result<(), PolicyError> {
        // The trap this project hits most often, and it degrades silently: a relative path
        // resolves against the *service's* working directory, so the audit trail is written
        // somewhere nobody looks and the gateway reports no error at all.
        if Path::new(path).is_relative() {
            return Err(PolicyError::Shape(
                "the audit path must be absolute: a relative one resolves against the service's \
                 working directory, so the file is written somewhere nobody is looking and nothing \
                 reports an error"
                    .into(),
            ));
        }
        self.set_scalar(&["audit", "jsonl", "path"], &quote_yaml(path))?;
        if self.parsed()?.audit.jsonl.path != path {
            return Err(PolicyError::NotApplied("audit.jsonl.path".into()));
        }
        Ok(())
    }

    /// Turn the CEF-over-syslog sink on or off.
    ///
    /// # Errors
    /// If the key cannot be located, or the result does not reparse to the requested value.
    pub fn set_syslog_enabled(&mut self, enabled: bool) -> Result<(), PolicyError> {
        self.set_scalar(&["audit", "syslog_cef", "enabled"], &enabled.to_string())?;
        if self.parsed()?.audit.syslog_cef.enabled != enabled {
            return Err(PolicyError::NotApplied("audit.syslog_cef.enabled".into()));
        }
        Ok(())
    }

    /// Where CEF events are sent.
    ///
    /// # Errors
    /// If the key cannot be located, or the result does not reparse to the requested value.
    pub fn set_syslog_address(&mut self, address: &str) -> Result<(), PolicyError> {
        self.set_scalar(&["audit", "syslog_cef", "address"], address)?;
        if self.parsed()?.audit.syslog_cef.address != address {
            return Err(PolicyError::NotApplied("audit.syslog_cef.address".into()));
        }
        Ok(())
    }

    /// Write the file back, keeping the previous contents beside it as `.bak`.
    ///
    /// The backup is taken from what is **on disk** rather than from what was read at startup, so
    /// a change somebody made in a text editor meanwhile is preserved rather than silently
    /// overwritten with a stale copy.
    ///
    /// # Errors
    /// If the current text does not parse, or the file cannot be written.
    pub fn save(&self) -> Result<PathBuf, PolicyError> {
        self.parsed()?;

        let backup = self.path.with_extension("yaml.bak");
        if self.path.exists() {
            std::fs::copy(&self.path, &backup).map_err(|source| PolicyError::Io {
                path: backup.clone(),
                source,
            })?;
        }
        std::fs::write(&self.path, self.text()).map_err(|source| PolicyError::Io {
            path: self.path.clone(),
            source,
        })?;
        Ok(backup)
    }

    /// The line index of a detector entry inside the `detectors:` block.
    fn detector_line(&self, key: &str) -> Option<usize> {
        let (start, end) = self.block(&["detectors"])?;
        (start..end).find(|&i| line_key(&self.lines[i]).is_some_and(|(_, k)| k == key))
    }

    /// Add a detector entry to the end of the `detectors:` block.
    fn insert_detector(&mut self, key: &str, mode: Decision) -> Result<(), PolicyError> {
        let (start, end) = self.block(&["detectors"]).ok_or_else(|| {
            PolicyError::Shape("this configuration has no `detectors:` block".into())
        })?;

        // Copy the shape of the neighbours rather than imposing one: these entries are aligned by
        // hand in every configuration this project ships, and an editor that breaks the alignment
        // every time somebody adds a detector makes the file look like it was machine-mangled.
        let template = (start..end)
            .rev()
            .find(|&i| line_key(&self.lines[i]).is_some())
            .ok_or_else(|| PolicyError::Shape("the `detectors:` block is empty".into()))?;
        let (indent, _) = line_key(&self.lines[template]).unwrap_or((String::new(), String::new()));
        let column = self.lines[template].find('{').unwrap_or(indent.len() + 14);
        let layer = match DataKind::ALL.iter().find(|k| detector_key(**k) == key) {
            Some(kind) if kind.layer() == sentin_core::Layer::Ner => "ner",
            _ => "deterministic",
        };

        let head = format!("{indent}{key}:");
        let pad = column.saturating_sub(head.len()).max(1);
        let carriage = if self.lines[template].ends_with('\r') {
            "\r"
        } else {
            ""
        };
        let line = format!(
            "{head}{:pad$}{{ layer: {layer}, mode: {} }}{carriage}",
            "",
            mode_word(mode),
            pad = pad
        );
        self.lines.insert(template + 1, line);
        Ok(())
    }

    /// Replace the value of a nested scalar key, leaving any trailing comment in place.
    fn set_scalar(&mut self, path: &[&str], value: &str) -> Result<(), PolicyError> {
        let (parent, key) = path.split_at(path.len() - 1);
        let key = key[0];
        let (start, end) = self
            .block(parent)
            .ok_or_else(|| PolicyError::Shape(format!("no `{}:` block", parent.join("."))))?;
        let index = (start..end)
            .find(|&i| line_key(&self.lines[i]).is_some_and(|(_, k)| k == key))
            .ok_or_else(|| PolicyError::Shape(format!("no `{}` key", path.join("."))))?;

        let line = &self.lines[index];
        let colon = line.find(':').ok_or_else(|| {
            PolicyError::Shape(format!("`{}` is not a key/value line", path.join(".")))
        })?;
        let rest = &line[colon + 1..];
        // A comment after the value is somebody's note about that setting. Rewriting the value is
        // asked for; discarding the note beside it is not.
        let tail = comment_tail(rest);
        self.lines[index] = format!("{}: {value}{tail}", &line[..colon]);
        Ok(())
    }

    /// The half-open line range holding the children of a nested block.
    fn block(&self, path: &[&str]) -> Option<(usize, usize)> {
        let mut start = 0usize;
        let mut end = self.lines.len();
        let mut depth: Option<usize> = None;

        for key in path {
            let index = (start..end).find(|&i| {
                line_key(&self.lines[i])
                    .is_some_and(|(indent, k)| k == *key && depth.is_none_or(|d| indent.len() > d))
            })?;
            let indent = line_key(&self.lines[index])?.0.len();
            depth = Some(indent);
            start = index + 1;
            // The block ends at the first line indented no further than its own key. Blank lines
            // and comments are skipped rather than ending it, because a comment at column zero
            // inside an indented block is ordinary in a hand-written file.
            end = (start..end)
                .find(|&i| {
                    let line = &self.lines[i];
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed.starts_with('#') {
                        return false;
                    }
                    line.len() - line.trim_start().len() <= indent
                })
                .unwrap_or(end);
        }
        Some((start, end))
    }
}

/// The indentation and key of a `key:` line, if it is one.
fn line_key(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
        return None;
    }
    let indent = line[..line.len() - trimmed.len()].to_string();
    let colon = trimmed.find(':')?;
    let key = trimmed[..colon].trim();
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    Some((indent, key.to_string()))
}

/// The `mode:` of a flow-mapping detector entry.
fn parse_mode(line: &str) -> Option<Decision> {
    let (_, value) = mode_span(line)?;
    match value {
        "observe" | "observed" => Some(Decision::Observed),
        "advise" | "advised" => Some(Decision::Advised),
        "mask" | "masked" => Some(Decision::Masked),
        "block" | "blocked" => Some(Decision::Blocked),
        _ => None,
    }
}

/// Rewrite just the `mode:` token of a detector line.
fn replace_mode(line: &str, mode: Decision) -> Option<String> {
    let (range, _) = mode_span(line)?;
    let mut out = String::with_capacity(line.len() + 4);
    out.push_str(&line[..range.start]);
    out.push_str(mode_word(mode));
    out.push_str(&line[range.end..]);
    Some(out)
}

/// Where the value of `mode:` sits inside a line, and what it currently says.
fn mode_span(line: &str) -> Option<(std::ops::Range<usize>, &str)> {
    let at = line.find("mode:")? + "mode:".len();
    let rest = &line[at..];
    let lead = rest.len() - rest.trim_start().len();
    let start = at + lead;
    let value = &line[start..];
    let len = value
        .find([',', '}', '#', '\r'])
        .unwrap_or(value.len())
        .min(value.len());
    let value = value[..len].trim_end();
    if value.is_empty() {
        return None;
    }
    Some((start..start + value.len(), value))
}

/// The imperative spelling, which is how a configuration reads: an instruction, not a record.
fn mode_word(mode: Decision) -> &'static str {
    match mode {
        Decision::Observed => "observe",
        Decision::Advised => "advise",
        Decision::Masked => "mask",
        Decision::Blocked => "block",
    }
}

/// Why a detector cannot reach a given verdict, in words an operator can act on.
fn why_not(kind: DataKind) -> &'static str {
    if kind.layer() == sentin_core::Layer::Ner {
        "it comes from the NER model, which is probabilistic, so it may advise or mask but never \
         refuse a request"
    } else {
        "it is matched by shape alone, with no checksum to verify it, and shape is not enough to \
         refuse somebody's request on"
    }
}

/// Whatever follows a value and must be carried across a rewrite: a trailing comment, and the
/// carriage return of a CRLF file.
///
/// The whitespace before the `#` is part of what is kept, and that is not cosmetic. YAML only
/// treats `#` as a comment when a space precedes it, so re-emitting `enabled: false# note` produces
/// the string `"false# note"` where a boolean was meant - a file that still parses as YAML and no
/// longer parses as a configuration.
///
/// A `#` that is *not* preceded by whitespace is left alone for the same reason: inside a quoted
/// Windows path it is an ordinary character, and treating it as a comment would truncate the value.
fn comment_tail(rest: &str) -> String {
    let bytes = rest.as_bytes();
    if let Some(at) = (1..bytes.len())
        .find(|&i| bytes[i] == b'#' && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t'))
    {
        let head = &rest[..at];
        let kept = head.len() - head.trim_end().len();
        return rest[at - kept..].to_string();
    }
    if rest.ends_with('\r') {
        "\r".to_string()
    } else {
        String::new()
    }
}

/// A double-quoted YAML scalar, with backslashes and quotes escaped.
///
/// Windows paths are the reason this exists: `C:\ProgramData\...` inside double quotes is a string
/// full of escape sequences, and `\P` is not one of them.
fn quote_yaml(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# A configuration with comments that must survive.
listen:
  host: 0.0.0.0
  port: 4141

detectors:
  pesel:        { layer: deterministic, mode: block }
  nip:          { layer: deterministic, mode: mask }
  email:        { layer: deterministic, mode: advise }
  person:       { layer: ner, mode: advise }

audit:
  jsonl:
    enabled: true
    path: "C:\\ProgramData\\Sentin-NPU\\audit.jsonl"
  syslog_cef:
    enabled: false
    address: 127.0.0.1:514
"#;

    fn sample() -> ConfigFile {
        ConfigFile::from_text(PathBuf::from("config.yaml"), SAMPLE).expect("sample parses")
    }

    #[test]
    fn a_file_that_is_not_touched_round_trips_byte_for_byte() {
        assert_eq!(sample().text(), SAMPLE);
    }

    #[test]
    fn crlf_survives_a_round_trip() {
        // An installed Windows configuration is CRLF. Converting it to LF on every save would be
        // an invisible change to every line of a file under version control at some sites.
        let crlf = SAMPLE.replace('\n', "\r\n");
        let file = ConfigFile::from_text(PathBuf::from("config.yaml"), &crlf).expect("parses");
        assert_eq!(file.text(), crlf);
    }

    #[test]
    fn changing_a_mode_changes_exactly_one_line() {
        let mut file = sample();
        file.set_detector_mode(DataKind::Nip, Decision::Blocked)
            .expect("nip has a checksum, so it may block");

        let before = SAMPLE.lines().collect::<Vec<_>>();
        let after = file.text();
        let after = after.lines().collect::<Vec<_>>();
        let differing: Vec<_> = before
            .iter()
            .zip(&after)
            .filter(|(a, b)| a != b)
            .map(|(_, b)| *b)
            .collect();
        assert_eq!(differing.len(), 1, "changed more than the one line");
        assert!(differing[0].contains("mode: block"));
        // The alignment and the layer are part of that line and were not the thing being changed.
        assert!(differing[0].starts_with("  nip:          { layer: deterministic,"));
    }

    #[test]
    fn comments_and_blank_lines_survive_an_edit() {
        let mut file = sample();
        file.set_detector_mode(DataKind::Pesel, Decision::Masked)
            .expect("pesel may be masked");
        let text = file.text();
        assert!(text.starts_with("# A configuration with comments that must survive."));
        assert!(text.contains("\n\ndetectors:"));
    }

    #[test]
    fn a_missing_detector_is_added_to_the_block() {
        let mut file = sample();
        assert_eq!(file.detector_mode(DataKind::VatEu), None);
        file.set_detector_mode(DataKind::VatEu, Decision::Masked)
            .expect("vat_eu may be masked");
        assert_eq!(file.detector_mode(DataKind::VatEu), Some(Decision::Masked));
        // Inside the block, not appended to the end of the file, where it would belong to `audit`.
        let text = file.text();
        let vat = text.find("vat_eu").expect("present");
        assert!(vat < text.find("audit:").expect("present"));
        assert!(text.contains("{ layer: deterministic, mode: mask }"));
    }

    #[test]
    fn an_added_ner_detector_is_labelled_as_one() {
        let mut file = sample();
        file.set_detector_mode(DataKind::Location, Decision::Observed)
            .expect("location may be observed");
        assert!(file.text().contains("location:") && file.text().contains("{ layer: ner,"));
    }

    #[test]
    fn a_detector_cannot_be_configured_beyond_its_evidence() {
        let mut file = sample();
        // Email has no checksum. The parser would accept `block` and the pipeline would clamp it
        // to masking at every request, which stores a policy the gateway does not run.
        let err = file
            .set_detector_mode(DataKind::Email, Decision::Blocked)
            .expect_err("shape alone must not block");
        assert!(format!("{err}").contains("checksum"), "{err}");
        assert_eq!(file.text(), SAMPLE, "a rejected edit must change nothing");

        let err = file
            .set_detector_mode(DataKind::Person, Decision::Blocked)
            .expect_err("the NER layer must not block");
        assert!(format!("{err}").contains("probabilistic"), "{err}");
    }

    #[test]
    fn every_kind_can_be_set_to_its_own_ceiling() {
        // The ceiling `sentin-core` advertises and the one this editor enforces have to be the
        // same, or an interface built from the first is rejected by the second.
        for kind in DataKind::ALL {
            let mut file = sample();
            file.set_detector_mode(kind, kind.max_decision())
                .unwrap_or_else(|e| panic!("{}: {e}", detector_key(kind)));
            assert_eq!(file.detector_mode(kind), Some(kind.max_decision()));
        }
    }

    #[test]
    fn a_windows_audit_path_is_quoted_so_it_survives_yaml() {
        let mut file = sample();
        file.set_audit_path("D:\\logs\\sentin\\audit.jsonl")
            .expect("absolute");
        assert_eq!(
            file.audit_jsonl().expect("parses").1,
            "D:\\logs\\sentin\\audit.jsonl"
        );
    }

    #[test]
    fn a_relative_audit_path_is_refused() {
        let mut file = sample();
        let err = file
            .set_audit_path("audit.jsonl")
            .expect_err("relative paths degrade silently");
        assert!(format!("{err}").contains("absolute"), "{err}");
    }

    #[test]
    fn audit_toggles_land_on_the_right_sink() {
        // `enabled:` appears twice in this file, under two different sinks. A search that took the
        // first match would turn the JSONL sink off when asked about syslog, and the file would
        // parse perfectly either way.
        let mut file = sample();
        file.set_syslog_enabled(true).expect("syslog toggles");
        let config = file.parsed().expect("parses");
        assert!(config.audit.syslog_cef.enabled);
        assert!(
            config.audit.jsonl.enabled,
            "the JSONL sink was switched by an edit aimed at syslog"
        );

        file.set_audit_enabled(false).expect("jsonl toggles");
        let config = file.parsed().expect("parses");
        assert!(!config.audit.jsonl.enabled);
        assert!(config.audit.syslog_cef.enabled);
    }

    #[test]
    fn a_comment_after_a_value_is_kept() {
        let text = SAMPLE.replace(
            "    enabled: true",
            "    enabled: true   # on by default, needs no infrastructure",
        );
        let mut file = ConfigFile::from_text(PathBuf::from("c.yaml"), &text).expect("parses");
        file.set_audit_enabled(false).expect("toggles");
        assert!(file
            .text()
            .contains("   # on by default, needs no infrastructure"));
        // And the space before it survived, or this would no longer be a comment at all.
        assert!(
            !file
                .parsed()
                .expect("still a configuration")
                .audit
                .jsonl
                .enabled
        );
    }

    #[test]
    fn a_hash_inside_a_quoted_path_is_not_a_comment() {
        // `#` is only a comment in YAML when whitespace precedes it. Treating every `#` as one
        // would truncate a path that legitimately contains it.
        let text = SAMPLE.replace(
            "    path: \"C:\\\\ProgramData\\\\Sentin-NPU\\\\audit.jsonl\"",
            "    path: \"C:\\\\logs\\\\team#4\\\\audit.jsonl\"",
        );
        let mut file = ConfigFile::from_text(PathBuf::from("c.yaml"), &text).expect("parses");
        file.set_audit_enabled(true).expect("toggles");
        assert_eq!(
            file.audit_jsonl().expect("parses").1,
            "C:\\logs\\team#4\\audit.jsonl"
        );
    }

    #[test]
    fn a_file_that_is_not_a_configuration_is_refused() {
        let err = ConfigFile::from_text(PathBuf::from("notes.txt"), "just: [some, text\n")
            .expect_err("invalid YAML");
        assert!(matches!(err, PolicyError::Parse(_)));
    }
}
