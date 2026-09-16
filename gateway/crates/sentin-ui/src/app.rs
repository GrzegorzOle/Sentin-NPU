// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! The window.
//!
//! Three tabs, and the order is the argument: what is protected, what was found, and what is
//! actually running. The third exists because every failure this project has shipped looked fine
//! from the outside - the service was running, the port answered, the installer said "finished" -
//! and what disagreed each time was one narrow check. A console that only showed settings would be
//! one more surface reporting success.
//!
//! Nothing here reaches the network, opens a file the operator did not name, or edits anything but
//! the configuration it was pointed at.

use std::path::PathBuf;

use eframe::egui;
use sentin_core::{DataKind, Decision, Layer, Validation};

use crate::policy::ConfigFile;
use crate::report::Report;
use crate::service::{self, Layer2, State};
use crate::text::Lang;

/// Which tab is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    /// Detector modes and the audit sinks.
    Protection,
    /// Building an HTML report from the audit trail.
    Report,
    /// What the service and the gateway's log say.
    Status,
}

/// Something to tell the operator, and whether it went well.
#[derive(Debug, Clone)]
struct Notice {
    text: String,
    good: bool,
}

/// The console application.
pub struct App {
    lang: Lang,
    tab: Tab,
    config_path: PathBuf,
    /// The configuration being edited, and the text last known to be on disk.
    file: Option<ConfigFile>,
    saved_text: String,
    open_error: Option<String>,
    audit_path: String,
    audit_enabled: bool,
    syslog_enabled: bool,
    syslog_address: String,
    notice: Option<Notice>,
    report_path: Option<PathBuf>,
    report_summary: Option<Report>,
    state: State,
    layer2: Layer2,
    can_administer: bool,
    last_refresh: std::time::Instant,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("config_path", &self.config_path)
            .field("tab", &self.tab)
            .finish_non_exhaustive()
    }
}

impl App {
    /// Open a configuration and build the console around it.
    #[must_use]
    pub fn new(config_path: PathBuf, lang: Lang) -> Self {
        let mut app = Self {
            lang,
            tab: Tab::Protection,
            config_path,
            file: None,
            saved_text: String::new(),
            open_error: None,
            audit_path: String::new(),
            audit_enabled: true,
            syslog_enabled: false,
            syslog_address: "127.0.0.1:514".to_string(),
            notice: None,
            report_path: None,
            report_summary: None,
            state: State::Unknown,
            layer2: Layer2::Silent,
            can_administer: false,
            last_refresh: std::time::Instant::now(),
        };
        app.reload();
        app.refresh_status();
        app
    }

    /// Read the configuration from disk, discarding anything unsaved.
    fn reload(&mut self) {
        match ConfigFile::open(&self.config_path) {
            Ok(file) => {
                if let Ok(config) = file.parsed() {
                    self.audit_enabled = config.audit.jsonl.enabled;
                    self.audit_path = config.audit.jsonl.path;
                    self.syslog_enabled = config.audit.syslog_cef.enabled;
                    self.syslog_address = config.audit.syslog_cef.address;
                }
                self.saved_text = file.text();
                self.file = Some(file);
                self.open_error = None;
            }
            Err(e) => {
                self.open_error = Some(e.to_string());
                self.file = None;
            }
        }
    }

    /// Ask the system what is running.
    fn refresh_status(&mut self) {
        self.state = service::state();
        self.layer2 = service::layer2_from_log(&service::log_path(&self.config_path));
        self.can_administer = service::can_administer(&self.config_path);
        self.last_refresh = std::time::Instant::now();
    }

    /// Whether there is anything unsaved.
    fn dirty(&self) -> bool {
        self.file
            .as_ref()
            .is_some_and(|file| file.text() != self.saved_text)
    }

    /// Write the configuration and restart the gateway so it reads it.
    fn apply(&mut self) {
        let Some(file) = &mut self.file else { return };

        // The sinks are edited through the same validating path as the detectors rather than being
        // written straight out, so a relative audit path is refused here too.
        let mut failures = Vec::new();
        if let Err(e) = file.set_audit_enabled(self.audit_enabled) {
            failures.push(e.to_string());
        }
        if self.audit_enabled {
            if let Err(e) = file.set_audit_path(&self.audit_path) {
                failures.push(e.to_string());
            }
        }
        if let Err(e) = file.set_syslog_enabled(self.syslog_enabled) {
            failures.push(e.to_string());
        }
        if self.syslog_enabled {
            if let Err(e) = file.set_syslog_address(&self.syslog_address) {
                failures.push(e.to_string());
            }
        }
        if !failures.is_empty() {
            self.notice = Some(Notice {
                text: failures.join("\n"),
                good: false,
            });
            return;
        }

        let backup = match file.save() {
            Ok(backup) => backup,
            Err(e) => {
                self.notice = Some(Notice {
                    text: e.to_string(),
                    good: false,
                });
                return;
            }
        };
        self.saved_text = file.text();

        // Saving is not applying. The gateway reads its configuration once, at startup, so a
        // console that stopped here would leave the operator believing a change had taken effect.
        let restarted = if self.state == State::NotInstalled {
            Ok(())
        } else {
            service::restart()
        };
        self.refresh_status();

        self.notice = Some(match restarted {
            Ok(()) => Notice {
                text: format!(
                    "{} {} {}",
                    self.lang.saved_ok(),
                    self.lang.backup_note(),
                    backup.display()
                ),
                good: true,
            },
            Err(e) => Notice {
                text: format!("{}\n{e}", self.lang.saved_no_restart()),
                good: false,
            },
        });
    }

    /// Start again with the rights to write the configuration, and close this window.
    ///
    /// Only on success: a refused prompt must leave the console where it was, or declining the
    /// elevation would look like the program crashing.
    fn elevate(&mut self, ctx: &egui::Context) {
        match service::relaunch_elevated(&self.config_path) {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => {
                self.notice = Some(Notice {
                    text: format!("{}\n{e}", self.lang.elevate_failed()),
                    good: false,
                });
            }
        }
    }

    /// Build the HTML report beside the audit file.
    fn build_report(&mut self) {
        let source = PathBuf::from(&self.audit_path);
        let text = match std::fs::read_to_string(&source) {
            Ok(text) => text,
            Err(e) => {
                self.notice = Some(Notice {
                    text: format!("{}: {e}", source.display()),
                    good: false,
                });
                return;
            }
        };
        let report = Report::from_jsonl(&text);
        if report.is_empty() {
            self.report_summary = None;
            self.notice = Some(Notice {
                text: self.lang.report_empty().to_string(),
                good: false,
            });
            return;
        }

        let out = source.with_file_name("sentin-report.html");
        let html = report.to_html(self.lang, &source.to_string_lossy());
        match std::fs::write(&out, html) {
            Ok(()) => {
                self.notice = Some(Notice {
                    text: format!("{} {}", self.lang.report_done(), out.display()),
                    good: true,
                });
                self.report_path = Some(out);
                self.report_summary = Some(report);
            }
            Err(e) => {
                self.notice = Some(Notice {
                    text: format!("{}: {e}", out.display()),
                    good: false,
                });
            }
        }
    }

    fn ui_protection(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        ui.label(lang.protection_intro());
        ui.add_space(8.0);

        if !self.can_administer {
            ui.colored_label(
                egui::Color32::from_rgb(0xb0, 0x4a, 0x12),
                lang.status_elevate(),
            );
            ui.add_space(4.0);
            if ui.button(lang.elevate_button()).clicked() {
                self.elevate(ui.ctx());
            }
            ui.add_space(6.0);
        }

        let Some(file) = &mut self.file else { return };

        // One width for every label on this tab, measured rather than guessed, so the pickers and
        // the text fields all start at the same place. Measuring is what makes it survive the
        // language button: the Polish and English labels are nothing like the same length.
        let column = label_column_width(ui, lang);

        // No max_height here. The action bar is a bottom panel of its own, so the height this Ui
        // reports already excludes it - reserving a second, hand-guessed 120 px for it left a dead
        // band above the buttons and, worse, cut the last field off at the edge of a viewport that
        // was shorter than the space available to it.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // One grid for all three groups, headings included as rows of it. Three grids sized
                // their first column three times, each to its own widest label, which is what put
                // the three blocks of pickers at three different left edges.
                egui::Grid::new("detectors")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .min_col_width(column)
                    .striped(true)
                    .show(ui, |ui| {
                        for (heading, group) in [
                            (lang.group_checksum(), Group::Checksum),
                            (lang.group_pattern(), Group::Pattern),
                            (lang.group_model(), Group::Model),
                        ] {
                            ui.vertical(|ui| {
                                ui.add_space(6.0);
                                ui.strong(heading);
                            });
                            ui.end_row();
                            for kind in DataKind::ALL.iter().copied().filter(|k| group.holds(*k)) {
                                ui.label(lang.detector_label(kind));
                                detector_combo(ui, lang, file, kind);
                                ui.end_row();
                            }
                        }
                    });

                ui.add_space(14.0);
                ui.strong(lang.audit_section());
                ui.label(lang.audit_note());
                ui.add_space(4.0);
                // Each switch keeps the field it governs directly beneath it - a grid apiece rather
                // than both switches and then both fields, which would leave the pairing to be
                // inferred from the order. The shared column is what keeps them aligned anyway.
                ui.checkbox(&mut self.audit_enabled, lang.audit_enabled());
                egui::Grid::new("audit-file")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .min_col_width(column)
                    .show(ui, |ui| {
                        ui.label(lang.audit_path());
                        ui.add_enabled(
                            self.audit_enabled,
                            egui::TextEdit::singleline(&mut self.audit_path).desired_width(420.0),
                        );
                        ui.end_row();
                    });
                ui.add_space(4.0);
                ui.checkbox(&mut self.syslog_enabled, lang.syslog_enabled());
                egui::Grid::new("audit-syslog")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .min_col_width(column)
                    .show(ui, |ui| {
                        ui.label(lang.syslog_address());
                        ui.add_enabled(
                            self.syslog_enabled,
                            egui::TextEdit::singleline(&mut self.syslog_address)
                                .desired_width(240.0),
                        );
                        ui.end_row();
                    });
                // A scroll area ends exactly where its content does, so without this the last
                // field sits flush against the bottom edge and reads as cut off even when it is
                // whole.
                ui.add_space(12.0);
            });
    }

    fn ui_report(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        ui.label(lang.report_intro());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(lang.report_source());
            ui.add(egui::TextEdit::singleline(&mut self.audit_path).desired_width(420.0));
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(lang.report_build()).clicked() {
                self.build_report();
            }
            let has = self.report_path.is_some();
            if ui
                .add_enabled(has, egui::Button::new(lang.report_open()))
                .clicked()
            {
                if let Some(path) = &self.report_path {
                    open_in_browser(path);
                }
            }
        });

        if let Some(report) = &self.report_summary {
            ui.add_space(12.0);
            ui.separator();
            egui::Grid::new("report-summary")
                .num_columns(2)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    ui.label(lang.report_period());
                    ui.label(format!("{} .. {}", report.first_ts, report.last_ts));
                    ui.end_row();
                    ui.label(lang.report_events());
                    ui.label(report.events.to_string());
                    ui.end_row();
                    for (label, value) in [
                        ("findings", report.findings()),
                        ("blocked", report.blocked),
                        ("masked", report.masked),
                    ] {
                        ui.label(label);
                        ui.label(value.to_string());
                        ui.end_row();
                    }
                });
        }
    }

    fn ui_status(&mut self, ui: &mut egui::Ui) {
        let lang = self.lang;
        egui::Grid::new("status")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.label(lang.status_service());
                ui.label(match self.state {
                    State::Running => lang.status_running(),
                    State::Stopped => lang.status_stopped(),
                    State::NotInstalled => lang.status_absent(),
                    State::Unknown => lang.status_unknown(),
                });
                ui.end_row();
                ui.label(lang.status_config());
                ui.label(self.config_path.display().to_string());
                ui.end_row();
                ui.label(lang.status_log());
                ui.label(service::log_path(&self.config_path).display().to_string());
                ui.end_row();
            });

        ui.add_space(12.0);
        ui.separator();
        ui.label(lang.status_layer2());
        ui.add_space(4.0);
        // The point of this panel: "the service is running" and "the service is inspecting what it
        // claims to" are different statements, and this project has shipped the first without the
        // second more than once.
        match &self.layer2 {
            Layer2::Ready(device) => {
                ui.colored_label(
                    egui::Color32::from_rgb(0x1a, 0x7f, 0x37),
                    format!("{} {device}", lang.status_layer2_ready()),
                );
            }
            Layer2::Unavailable => {
                ui.colored_label(
                    egui::Color32::from_rgb(0xb0, 0x2a, 0x2a),
                    lang.status_layer2_missing(),
                );
            }
            Layer2::Silent => {
                ui.label(lang.status_layer2_silent());
            }
        }

        ui.add_space(14.0);
        if ui
            .add_enabled(
                self.state != State::NotInstalled,
                egui::Button::new(lang.status_restart()),
            )
            .clicked()
        {
            let outcome = service::restart();
            self.refresh_status();
            self.notice = Some(match outcome {
                Ok(()) => Notice {
                    text: lang.saved_ok().to_string(),
                    good: true,
                },
                Err(e) => Notice {
                    text: e,
                    good: false,
                },
            });
        }
    }
}

/// Which section of the protection tab a detector belongs in.
#[derive(Clone, Copy)]
enum Group {
    Checksum,
    Pattern,
    Model,
}

impl Group {
    fn holds(self, kind: DataKind) -> bool {
        match self {
            Group::Checksum => {
                kind.layer() == Layer::Deterministic && kind.evidence() == Validation::Checksum
            }
            Group::Pattern => {
                kind.layer() == Layer::Deterministic && kind.evidence() == Validation::Pattern
            }
            Group::Model => kind.layer() == Layer::Ner,
        }
    }
}

/// How wide the left-hand column of the protection tab has to be.
///
/// Measured from the strings that will actually be drawn, in the language that is actually
/// selected, because every alternative drifts: a constant goes stale the first time a label is
/// reworded, and letting each grid size itself is what staggered the three blocks of pickers in the
/// first place. Headings are measured too - they share the column, so one long heading would widen
/// it past whatever the labels asked for and the alignment would come apart again.
fn label_column_width(ui: &egui::Ui, lang: Lang) -> f32 {
    left_column_labels(lang)
        .into_iter()
        .map(|text| text_width(ui, text))
        .fold(0.0_f32, f32::max)
}

/// Every string that is drawn in that left-hand column, in one place.
///
/// Kept as a list rather than measured where each is drawn, so that adding a row means adding it
/// here too - a row whose label is wider than the column is the one that would push its own field
/// out of line and reintroduce exactly the stagger this replaced.
fn left_column_labels(lang: Lang) -> Vec<&'static str> {
    DataKind::ALL
        .iter()
        .map(|kind| lang.detector_label(*kind))
        .chain([
            lang.group_checksum(),
            lang.group_pattern(),
            lang.group_model(),
            lang.audit_path(),
            lang.syslog_address(),
        ])
        .collect()
}

/// How wide a piece of body text is once laid out.
fn text_width(ui: &egui::Ui, text: &str) -> f32 {
    let style = egui::TextStyle::Body.resolve(ui.style());
    // fonts_mut, not fonts: laying text out memoizes the galley, so the call needs the cache
    // mutably even though nothing about the fonts themselves changes. That memoization is also why
    // measuring every label on every frame costs nothing after the first.
    ui.ctx().fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(text.to_owned(), style, egui::Color32::PLACEHOLDER)
            .size()
            .x
    })
}

/// The mode picker for one detector.
///
/// It offers only what the detector can actually do. Offering "refuse the request" against an email
/// address would be offering a misunderstanding: the parser accepts it and the pipeline clamps it
/// at every request, so the operator would be told one thing and sold another.
fn detector_combo(ui: &mut egui::Ui, lang: Lang, file: &mut ConfigFile, kind: DataKind) {
    let current = file.detector_mode(kind);
    let selected = current.unwrap_or(Decision::Observed);
    let label = match current {
        Some(mode) => lang.mode_label(mode).to_string(),
        None => format!("{} ({})", lang.mode_label(selected), lang.unset_note()),
    };

    egui::ComboBox::from_id_salt(kind as u8)
        .selected_text(label)
        .width(240.0)
        .show_ui(ui, |ui| {
            for mode in [
                Decision::Observed,
                Decision::Advised,
                Decision::Masked,
                Decision::Blocked,
            ] {
                if mode > kind.max_decision() {
                    continue;
                }
                let mut chosen = current == Some(mode);
                if ui
                    .selectable_label(chosen, lang.mode_label(mode))
                    .on_hover_text(lang.mode_help(mode))
                    .clicked()
                {
                    chosen = true;
                }
                if chosen && current != Some(mode) {
                    // A rejected edit leaves the file untouched, so a failure here cannot half-apply.
                    let _ = file.set_detector_mode(kind, mode);
                }
            }
        });
}

/// Hand a file to whatever the desktop uses to open it.
fn open_in_browser(path: &std::path::Path) {
    #[cfg(windows)]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .spawn();
    #[cfg(not(windows))]
    let result = std::process::Command::new("xdg-open").arg(path).spawn();
    let _ = result;
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Cheap and infrequent: the service state is queried a few times a minute, not every frame.
        if self.last_refresh.elapsed() > std::time::Duration::from_secs(5) {
            self.refresh_status();
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(5));

        egui::Panel::top(egui::Id::new("tabs")).show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Protection, self.lang.tab_protection());
                ui.selectable_value(&mut self.tab, Tab::Report, self.lang.tab_report());
                ui.selectable_value(&mut self.tab, Tab::Status, self.lang.tab_status());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(self.lang.other().name()).clicked() {
                        self.lang = self.lang.other();
                    }
                });
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom(egui::Id::new("actions")).show(ui, |ui| {
            ui.add_space(6.0);
            if let Some(notice) = &self.notice {
                let colour = if notice.good {
                    egui::Color32::from_rgb(0x1a, 0x7f, 0x37)
                } else {
                    egui::Color32::from_rgb(0xb0, 0x2a, 0x2a)
                };
                ui.colored_label(colour, &notice.text);
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                let dirty = self.dirty()
                    || self.tab == Tab::Protection && self.file.is_some() && self.sinks_changed();
                if ui
                    .add_enabled(
                        dirty && self.can_administer,
                        egui::Button::new(self.lang.apply()),
                    )
                    .clicked()
                {
                    self.apply();
                }
                if ui
                    .add_enabled(dirty, egui::Button::new(self.lang.revert()))
                    .clicked()
                {
                    self.reload();
                    self.notice = None;
                }
                if !dirty {
                    ui.label(self.lang.pending_none());
                }
            });
            ui.add_space(6.0);
        });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            if let Some(error) = self.open_error.clone() {
                ui.colored_label(egui::Color32::from_rgb(0xb0, 0x2a, 0x2a), error);
                return;
            }
            match self.tab {
                Tab::Protection => self.ui_protection(ui),
                Tab::Report => self.ui_report(ui),
                Tab::Status => self.ui_status(ui),
            }
        });
    }
}

impl App {
    /// Whether the audit fields differ from what the file on disk says.
    fn sinks_changed(&self) -> bool {
        let Some(file) = &self.file else { return false };
        let Ok(config) = file.parsed() else {
            return false;
        };
        config.audit.jsonl.enabled != self.audit_enabled
            || config.audit.jsonl.path != self.audit_path
            || config.audit.syslog_cef.enabled != self.syslog_enabled
            || config.audit.syslog_cef.address != self.syslog_address
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run one headless frame with real fonts.
    ///
    /// Not `egui::__run_test_ui`, which loads an empty font set to save time - every string would
    /// then measure zero and a width test would pass by measuring nothing.
    fn with_ui(f: impl FnMut(&mut egui::Ui)) {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), f)
            .drop_without_applying_deltas();
    }

    #[test]
    fn the_column_holds_every_label_in_both_languages() {
        with_ui(|ui| {
            for lang in [Lang::En, Lang::Pl] {
                let column = label_column_width(ui, lang);
                assert!(column > 0.0, "{lang:?}: measured nothing");
                for label in left_column_labels(lang) {
                    let width = text_width(ui, label);
                    assert!(
                        width <= column,
                        "{lang:?}: {label:?} is {width} wide against a {column} column, so its \
                         field would sit further right than every other one",
                    );
                }
            }
        });
    }

    #[test]
    fn every_detector_and_both_sink_labels_are_measured() {
        // The stagger came from a column sized without knowing about some of the rows in it. This
        // fails if a detector is added to DataKind::ALL and nothing else, which is how it would
        // come back.
        for lang in [Lang::En, Lang::Pl] {
            let labels = left_column_labels(lang);
            for kind in DataKind::ALL {
                assert!(
                    labels.contains(&lang.detector_label(kind)),
                    "{lang:?}: {kind:?} is drawn in that column and is not measured for it",
                );
            }
            assert!(labels.contains(&lang.audit_path()));
            assert!(labels.contains(&lang.syslog_address()));
        }
    }
}
