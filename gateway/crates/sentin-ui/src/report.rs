// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Turning the JSONL audit trail into one self-contained HTML file.
//!
//! A station wired to a SIEM already has this: rules, a dashboard, and somebody whose job it is to
//! read them. A station without one has a file of JSON objects that nobody is going to read, and
//! therefore has no answer to the only question anyone actually asks - *did this help, and what did
//! it catch?* That gap is what this module closes, on the side of the fence where the Wazuh package
//! cannot reach.
//!
//! Three properties are deliberate:
//!
//! - **No dependency renders the charts.** The SVG is written directly, exactly as
//!   `tools/bench/plot.py` already does for the benchmark charts, so a chart change reads as a text
//!   diff and nothing needs installing to produce one.
//! - **The reader is tolerant of its own history.** An audit file on an upgraded machine holds
//!   events from several schema versions: the oldest carry no `device`, later ones no `client_addr`
//!   or `source`, and only the newest carry `attachment_sha256`. A reader that insisted on the
//!   current schema would quietly report on the tail of the file and present it as the whole
//!   period.
//! - **The report cannot leak what the audit trail refuses to record.** Every value printed here is
//!   one the gateway chose to write - a detector name, a verdict, a digest - because the event type
//!   has no field that could hold detected text. That is what makes the file safe to email, and the
//!   report says so itself rather than leaving the reader to assume it.

use std::collections::{BTreeMap, BTreeSet};

use crate::text::Lang;

/// One line of the audit trail, read as permissively as possible.
#[derive(Debug, Clone, serde::Deserialize)]
struct Record {
    #[serde(default)]
    ts: String,
    #[serde(default)]
    event: String,
    #[serde(default)]
    detector: Option<String>,
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    upstream_model: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    client_addr: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    attachment_kind: Option<String>,
    #[serde(default)]
    attachment_sha256: Option<String>,
}

/// A counted breakdown.
pub type Tally = BTreeMap<String, usize>;

/// Everything the report says, computed once from the audit trail.
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// How many lines parsed as events.
    pub events: usize,
    /// How many lines did not parse.
    ///
    /// Reported rather than hidden: a non-zero count means a truncated write or a file that is not
    /// an audit trail, and a summary that silently skipped them would describe a different period
    /// from the one it claims.
    pub malformed: usize,
    /// Timestamp of the earliest event.
    pub first_ts: String,
    /// Timestamp of the latest event.
    pub last_ts: String,
    /// Findings per detector.
    pub detectors: Tally,
    /// Findings per verdict.
    pub decisions: Tally,
    /// Findings per inference device.
    pub devices: Tally,
    /// Events per upstream model.
    pub models: Tally,
    /// Events per adapter.
    pub providers: Tally,
    /// Events per calling address.
    pub callers: Tally,
    /// Findings by where they were: typed into the prompt, or inside an attachment.
    pub sources: Tally,
    /// Attachments by the format they turned out to be.
    pub attachment_kinds: Tally,
    /// Findings per day.
    pub daily: Tally,
    /// Findings that refused the request outright.
    pub blocked: usize,
    /// Findings whose value was replaced before the request left the machine.
    pub masked: usize,
    /// How many distinct documents were seen, counted by digest.
    pub distinct_attachments: usize,
    /// Attachments the gateway could not read, so nothing inside them was inspected.
    pub skipped_attachments: usize,
}

impl Report {
    /// Build a report from the contents of an audit file.
    #[must_use]
    pub fn from_jsonl(text: &str) -> Self {
        let mut report = Self::default();
        let mut digests = BTreeSet::new();

        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<Record>(line) else {
                report.malformed += 1;
                continue;
            };
            report.events += 1;

            if !record.ts.is_empty() {
                if report.first_ts.is_empty() || record.ts < report.first_ts {
                    report.first_ts.clone_from(&record.ts);
                }
                if record.ts > report.last_ts {
                    report.last_ts.clone_from(&record.ts);
                }
            }

            match record.event.as_str() {
                "pii_detected" => {
                    bump(&mut report.detectors, record.detector.as_deref());
                    bump(&mut report.decisions, record.decision.as_deref());
                    bump(&mut report.devices, record.device.as_deref());
                    // An event without `source` predates the distinction rather than describing a
                    // finding that was neither typed nor attached.
                    bump(
                        &mut report.sources,
                        Some(record.source.as_deref().unwrap_or("prompt")),
                    );
                    bump(
                        &mut report.attachment_kinds,
                        record.attachment_kind.as_deref(),
                    );
                    if let Some(digest) = &record.attachment_sha256 {
                        digests.insert(digest.clone());
                    }
                    if let Some(day) = record.ts.get(..10) {
                        *report.daily.entry(day.to_string()).or_default() += 1;
                    }
                    match record.decision.as_deref() {
                        Some("blocked") => report.blocked += 1,
                        Some("masked") => report.masked += 1,
                        _ => {}
                    }
                }
                "attachment_skipped" => {
                    report.skipped_attachments += 1;
                    bump(
                        &mut report.attachment_kinds,
                        record.attachment_kind.as_deref(),
                    );
                }
                _ => {}
            }

            bump(&mut report.models, record.upstream_model.as_deref());
            bump(&mut report.providers, record.provider.as_deref());
            bump(&mut report.callers, record.client_addr.as_deref());
        }

        report.distinct_attachments = digests.len();
        report
    }

    /// Whether there is anything to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events == 0
    }

    /// How many findings were recorded, across every detector.
    #[must_use]
    pub fn findings(&self) -> usize {
        self.detectors.values().sum()
    }

    /// Render the whole report as one self-contained HTML document.
    #[must_use]
    pub fn to_html(&self, lang: Lang, source: &str) -> String {
        let t = Wording::for_lang(lang);
        let period = if self.first_ts.is_empty() {
            "-".to_string()
        } else {
            format!("{} .. {}", self.first_ts, self.last_ts)
        };

        let mut body = String::new();
        body.push_str(&format!(
            "<header><h1>{}</h1><p class=\"sub\">{} <code>{}</code></p>\
             <p class=\"sub\">{}: {}</p></header>\n",
            esc(t.title),
            esc(t.source),
            esc(source),
            esc(t.period),
            esc(&period)
        ));

        body.push_str("<section class=\"cards\">\n");
        for (label, value) in [
            (t.card_events, self.events.to_string()),
            (t.card_findings, self.findings().to_string()),
            (t.card_blocked, self.blocked.to_string()),
            (t.card_masked, self.masked.to_string()),
            (t.card_attachments, self.distinct_attachments.to_string()),
        ] {
            body.push_str(&format!(
                "<div class=\"card\"><div class=\"n\">{}</div><div class=\"l\">{}</div></div>\n",
                esc(&value),
                esc(label)
            ));
        }
        body.push_str("</section>\n");

        body.push_str(&format!("<p class=\"note\">{}</p>\n", esc(t.privacy)));

        body.push_str(&chart_section(t.h_detectors, &self.detectors, 0));
        body.push_str(&chart_section(t.h_daily, &self.daily, 1));

        body.push_str(&table_section(
            t.h_decisions,
            t.col_verdict,
            &self.decisions,
        ));
        body.push_str(&table_section(t.h_sources, t.col_where, &self.sources));
        body.push_str(&table_section(t.h_models, t.col_model, &self.models));
        body.push_str(&table_section(t.h_callers, t.col_caller, &self.callers));
        body.push_str(&table_section(t.h_devices, t.col_device, &self.devices));
        if !self.attachment_kinds.is_empty() {
            body.push_str(&table_section(
                t.h_attachments,
                t.col_kind,
                &self.attachment_kinds,
            ));
        }

        if self.skipped_attachments > 0 {
            body.push_str(&format!(
                "<p class=\"warn\">{} {}</p>\n",
                self.skipped_attachments,
                esc(t.skipped)
            ));
        }
        if self.malformed > 0 {
            body.push_str(&format!(
                "<p class=\"warn\">{} {}</p>\n",
                self.malformed,
                esc(t.malformed)
            ));
        }

        body.push_str(&format!(
            "<footer>{} {} - Sentin-NPU {}</footer>\n",
            esc(t.generated),
            esc(&now_utc()),
            esc(env!("CARGO_PKG_VERSION"))
        ));

        format!(
            "<!DOCTYPE html>\n<html lang=\"{}\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
             <title>{}</title>\n<style>{}</style>\n</head>\n<body>\n{}</body>\n</html>\n",
            match lang {
                Lang::En => "en",
                Lang::Pl => "pl",
            },
            esc(t.title),
            STYLE,
            body
        )
    }
}

/// Count one occurrence, ignoring an absent or empty value.
fn bump(tally: &mut Tally, value: Option<&str>) {
    if let Some(value) = value {
        if !value.is_empty() {
            *tally.entry(value.to_string()).or_default() += 1;
        }
    }
}

/// A heading, a bar chart and the same numbers as a table beside it.
///
/// The table is not redundant. It is the accessible twin of the chart - the same convention the
/// benchmark charts follow - and it is what survives being pasted into an email that strips the
/// SVG.
fn chart_section(heading: &str, tally: &Tally, series: usize) -> String {
    if tally.is_empty() {
        return String::new();
    }
    format!(
        "<section><h2>{}</h2>{}</section>\n",
        esc(heading),
        bar_chart(tally, series)
    )
}

/// A horizontal bar chart as inline SVG.
fn bar_chart(tally: &Tally, series: usize) -> String {
    let mut rows: Vec<(&String, &usize)> = tally.iter().collect();
    // Biggest first, ties broken by name so two runs of the same data render identically.
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

    let max = rows.first().map_or(1, |(_, v)| **v).max(1);
    let label_w = 190.0_f64;
    let bar_w = 470.0_f64;
    let row_h = 26.0_f64;
    let height = row_h * rows.len() as f64 + 16.0;
    let colour = SERIES[series % SERIES.len()];

    let mut svg = format!(
        "<svg class=\"chart\" viewBox=\"0 0 {:.0} {:.0}\" width=\"100%\" height=\"{:.0}\" \
         role=\"img\" xmlns=\"http://www.w3.org/2000/svg\">",
        label_w + bar_w + 60.0,
        height,
        height
    );
    for (index, (label, value)) in rows.iter().enumerate() {
        let y = 8.0 + row_h * index as f64;
        let w = (bar_w * (**value as f64) / max as f64).max(2.0);
        svg.push_str(&format!(
            "<text x=\"{:.0}\" y=\"{:.1}\" font-size=\"12\" fill=\"{INK}\" \
             text-anchor=\"end\" font-family=\"{FONT}\">{}</text>",
            label_w - 8.0,
            y + 14.0,
            esc(label)
        ));
        svg.push_str(&format!(
            "<rect x=\"{label_w:.0}\" y=\"{:.1}\" width=\"{w:.1}\" height=\"16\" rx=\"2\" \
             fill=\"{colour}\"/>",
            y + 3.0
        ));
        svg.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"12\" fill=\"{INK_SECONDARY}\" \
             font-family=\"{FONT}\">{}</text>",
            label_w + w + 6.0,
            y + 14.0,
            value
        ));
    }
    svg.push_str("</svg>");
    svg
}

/// A heading and a two-column table.
fn table_section(heading: &str, column: &str, tally: &Tally) -> String {
    if tally.is_empty() {
        return String::new();
    }
    let mut rows: Vec<(&String, &usize)> = tally.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

    let mut html = format!(
        "<section><h2>{}</h2><table><thead><tr><th>{}</th><th class=\"n\">n</th></tr></thead>\
         <tbody>",
        esc(heading),
        esc(column)
    );
    for (label, value) in rows {
        html.push_str(&format!(
            "<tr><td>{}</td><td class=\"n\">{value}</td></tr>",
            esc(label)
        ));
    }
    html.push_str("</tbody></table></section>\n");
    html
}

/// Escape text for both HTML and the XML inside the SVG.
///
/// One function for both on purpose: the charts live inside the document, so a value escaped for
/// one context and not the other produces a file that renders in a browser and fails any XML
/// parser - which is the shape of bug that once produced five unparseable benchmark charts.
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The current UTC time, formatted like the timestamps in the audit trail.
///
/// Written out rather than pulled from a date library: one format, one direction, no locale, and
/// the crate stays free of a dependency whose whole surface would go unused.
fn now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since the Unix epoch to a calendar date. Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const FONT: &str = "system-ui, -apple-system, Segoe UI, Roboto, sans-serif";
const INK: &str = "#0b0b0b";
const INK_SECONDARY: &str = "#52514e";
/// Blue then orange, in fixed order and never cycled - the same rule the benchmark charts follow.
const SERIES: [&str; 2] = ["#2a78d6", "#eb6834"];

/// Light surface only: GitHub's dark theme does not follow the operating system preference, and a
/// report that guesses wrong is unreadable in exactly the situation it is being shown to somebody.
const STYLE: &str = "\
:root{color-scheme:light}\
body{background:#fcfcfb;color:#0b0b0b;font-family:system-ui,-apple-system,Segoe UI,Roboto,sans-serif;\
margin:0 auto;max-width:900px;padding:32px 20px 64px;line-height:1.5}\
h1{font-size:22px;margin:0 0 4px}h2{font-size:15px;margin:28px 0 8px;font-weight:600}\
.sub{color:#52514e;margin:2px 0;font-size:13px}\
code{background:#f0efec;padding:1px 4px;border-radius:3px;font-size:12px}\
.cards{display:flex;flex-wrap:wrap;gap:10px;margin:20px 0 8px}\
.card{background:#fff;border:1px solid #e1e0d9;border-radius:6px;padding:10px 14px;min-width:104px}\
.card .n{font-size:22px;font-weight:600}.card .l{color:#52514e;font-size:12px}\
.note{background:#f4f7fc;border-left:3px solid #2a78d6;padding:10px 12px;font-size:13px;margin:16px 0}\
.warn{background:#fdf3ee;border-left:3px solid #eb6834;padding:10px 12px;font-size:13px}\
table{border-collapse:collapse;width:100%;font-size:13px}\
th,td{text-align:left;padding:5px 8px;border-bottom:1px solid #e1e0d9}\
th{color:#52514e;font-weight:600}td.n,th.n{text-align:right;font-variant-numeric:tabular-nums}\
.chart{display:block;margin:4px 0 8px}\
footer{color:#898781;font-size:12px;margin-top:36px;border-top:1px solid #e1e0d9;padding-top:10px}";

/// The report's own wording, kept beside the interface wording but separate from it: this text
/// ends up in a file somebody else reads, possibly months later and without the console open.
struct Wording {
    title: &'static str,
    source: &'static str,
    period: &'static str,
    card_events: &'static str,
    card_findings: &'static str,
    card_blocked: &'static str,
    card_masked: &'static str,
    card_attachments: &'static str,
    privacy: &'static str,
    h_detectors: &'static str,
    h_daily: &'static str,
    h_decisions: &'static str,
    h_sources: &'static str,
    h_models: &'static str,
    h_callers: &'static str,
    h_devices: &'static str,
    h_attachments: &'static str,
    col_verdict: &'static str,
    col_where: &'static str,
    col_model: &'static str,
    col_caller: &'static str,
    col_device: &'static str,
    col_kind: &'static str,
    skipped: &'static str,
    malformed: &'static str,
    generated: &'static str,
}

impl Wording {
    fn for_lang(lang: Lang) -> Self {
        match lang {
            Lang::En => Self {
                title: "Sentin-NPU activity report",
                source: "Audit trail:",
                period: "Period",
                card_events: "events",
                card_findings: "findings",
                card_blocked: "requests refused",
                card_masked: "identifiers hidden",
                card_attachments: "distinct documents",
                privacy: "This report is built from metadata only. The audit trail has no field \
                          that could hold a detected identifier, so nothing here reveals what was \
                          found - only that something was, of what kind, and what happened to it.",
                h_detectors: "What was found",
                h_daily: "Findings per day",
                h_decisions: "What happened to it",
                h_sources: "Typed, or inside an attachment",
                h_models: "Models the data was heading towards",
                h_callers: "Callers",
                h_devices: "Device that ran the inspection",
                h_attachments: "Attachment formats",
                col_verdict: "verdict",
                col_where: "where",
                col_model: "model",
                col_caller: "address",
                col_device: "device",
                col_kind: "format",
                skipped: "attachments could not be read, so nothing inside them was inspected.",
                malformed: "lines could not be parsed and are not counted anywhere above.",
                generated: "Generated",
            },
            Lang::Pl => Self {
                title: "Sentin-NPU - raport z działania",
                source: "Dziennik zdarzeń:",
                period: "Okres",
                card_events: "zdarzeń",
                card_findings: "znalezisk",
                card_blocked: "odrzuconych żądań",
                card_masked: "ukrytych identyfikatorów",
                card_attachments: "różnych dokumentów",
                privacy: "Ten raport powstaje wyłącznie z metadanych. Dziennik nie ma pola, w \
                          którym mógłby się znaleźć wykryty identyfikator, wiec nic tutaj nie \
                          zdradza, co zostało znalezione - tylko że coś znaleziono, jakiego rodzaju \
                          i co się z tym stało.",
                h_detectors: "Co zostało znalezione",
                h_daily: "Znaleziska według dni",
                h_decisions: "Co się z tym stało",
                h_sources: "Wpisane w treść czy w załączniku",
                h_models: "Modele, do których zmierzały dane",
                h_callers: "Skąd przyszły żądania",
                h_devices: "Urządzenie, które wykonało inspekcję",
                h_attachments: "Formaty załączników",
                col_verdict: "decyzja",
                col_where: "gdzie",
                col_model: "model",
                col_caller: "adres",
                col_device: "urządzenie",
                col_kind: "format",
                skipped: "załączników nie dało się odczytać, więc ich zawartość nie była sprawdzona.",
                malformed: "wierszy nie dało się odczytać i nie są liczone w żadnym z powyższych \
                            zestawień.",
                generated: "Wygenerowano",
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUDIT: &str = r#"
{"ts":"2026-09-16T06:57:10Z","event":"pii_detected","detector":"vat_eu","data_type":"VAT_EU","decision":"masked","device":"CPU","upstream_model":"local-qwen","provider":"openai","client_addr":"127.0.0.1","source":"prompt"}
{"ts":"2026-09-16T06:57:10Z","event":"pii_detected","detector":"pesel","data_type":"PESEL","decision":"blocked","device":"CPU","upstream_model":"local-qwen","provider":"openai","client_addr":"127.0.0.1","source":"attachment","attachment_kind":"pdf","attachment_sha256":"sha256:aa"}
{"ts":"2026-09-17T08:00:00Z","event":"pii_detected","detector":"pesel","data_type":"PESEL","decision":"blocked","device":"CPU","upstream_model":"local-qwen","provider":"openai","client_addr":"10.0.0.9","source":"attachment","attachment_kind":"pdf","attachment_sha256":"sha256:aa"}
{"ts":"2026-09-17T08:00:01Z","event":"attachment_skipped","attachment_kind":"opaque","client_addr":"10.0.0.9"}
{"ts":"2026-09-17T08:00:02Z","event":"decision_made","decision":"blocked","client_addr":"10.0.0.9","upstream_model":"local-qwen","provider":"openai"}
"#;

    #[test]
    fn the_headline_numbers_come_out_of_the_events() {
        let report = Report::from_jsonl(AUDIT);
        assert_eq!(report.events, 5);
        assert_eq!(report.findings(), 3);
        assert_eq!(report.blocked, 2);
        assert_eq!(report.masked, 1);
        assert_eq!(report.detectors.get("pesel"), Some(&2));
        assert_eq!(report.detectors.get("vat_eu"), Some(&1));
        assert_eq!(report.skipped_attachments, 1);
        assert_eq!(report.first_ts, "2026-09-16T06:57:10Z");
        assert_eq!(report.last_ts, "2026-09-17T08:00:02Z");
    }

    #[test]
    fn one_document_seen_twice_is_one_document() {
        // The whole reason the digest was added: two uploads of the same file from two
        // workstations were previously two unrelated rows.
        let report = Report::from_jsonl(AUDIT);
        assert_eq!(report.distinct_attachments, 1);
        assert_eq!(report.callers.len(), 2, "but from two different callers");
    }

    #[test]
    fn events_from_an_older_schema_still_count() {
        // A file appended to across upgrades holds events with no `device`, no `client_addr` and
        // no `source`. Counting only the modern ones would report on the tail of the file and
        // present it as the whole period.
        let old = r#"{"ts":"2026-08-01T10:00:00Z","event":"pii_detected","detector":"pesel","decision":"masked"}"#;
        let report = Report::from_jsonl(old);
        assert_eq!(report.events, 1);
        assert_eq!(report.findings(), 1);
        assert!(report.devices.is_empty(), "absent, not invented");
        assert_eq!(
            report.sources.get("prompt"),
            Some(&1),
            "an event older than the distinction is a prompt finding, not an unknown"
        );
    }

    #[test]
    fn a_line_that_is_not_an_event_is_counted_as_such() {
        let report = Report::from_jsonl("not json\n{\"ts\":\"x\",\"event\":\"pii_detected\"}\n");
        assert_eq!(report.malformed, 1);
        assert_eq!(report.events, 1);
    }

    #[test]
    fn an_empty_file_produces_an_empty_report() {
        let report = Report::from_jsonl("\n\n");
        assert!(report.is_empty());
        assert_eq!(report.malformed, 0);
    }

    #[test]
    fn the_report_is_well_formed_and_self_contained() {
        let html = Report::from_jsonl(AUDIT).to_html(Lang::En, "audit.jsonl");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<svg"));
        // Self-contained: nothing is fetched, so the file works from an email attachment or a USB
        // stick, which is exactly how a report from a machine with no SIEM reaches anybody. The
        // one URL in the document is the SVG namespace, which is an identifier and not an address
        // - the check is for things that would cause a request.
        assert!(!html.contains("src="));
        assert!(!html.contains("href="));
        assert!(!html.contains("@import"));
        assert_eq!(
            html.matches("http").count(),
            html.matches("www.w3.org").count()
        );
        assert!(!html.contains("<script"));
        // Balanced tags are cheap to assert and catch the class of mistake that produced five
        // unparseable charts once already.
        assert_eq!(
            html.matches("<section").count(),
            html.matches("</section>").count()
        );
        assert_eq!(html.matches("<svg").count(), html.matches("</svg>").count());
    }

    #[test]
    fn a_hostile_value_cannot_break_out_of_the_markup() {
        // Every value here is one the gateway wrote, but `upstream_model` is chosen by the caller,
        // so it is attacker-influenced text arriving in a file somebody opens in a browser.
        let line = r#"{"ts":"2026-09-16T06:57:10Z","event":"pii_detected","detector":"pesel","decision":"masked","upstream_model":"<script>alert(1)</script>"}"#;
        let html = Report::from_jsonl(line).to_html(Lang::Pl, "audit.jsonl");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn both_languages_render() {
        for lang in [Lang::En, Lang::Pl] {
            let html = Report::from_jsonl(AUDIT).to_html(lang, "audit.jsonl");
            assert!(html.contains("Sentin-NPU"));
            assert!(!html.contains('\u{2014}') && !html.contains('\u{2013}'));
        }
    }

    #[test]
    fn the_generated_date_is_a_real_date() {
        let now = now_utc();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z'));
        // A fixed point checked against the algorithm's definition rather than against itself.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }
}
