// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Interface wording, in English and Polish.
//!
//! The rest of this project is written in English, for the OpenVINO community it is published to.
//! This one crate is not for that reader. It is for the person in an office who has been told the
//! gateway protects them and now has to decide whether a tax number should be masked or refused -
//! and on the machines this was built for, that person works in Polish. An English-only console
//! would hand the decision straight back to whoever was comfortable with the YAML file, which is
//! the situation it exists to end.
//!
//! The wording is deliberately about consequences rather than field names: "the request is refused"
//! rather than `mode: block`. Every string is a function, so a missing translation is a compile
//! error rather than an English sentence appearing in the middle of a Polish window.

use sentin_core::{DataKind, Decision};

/// Which language the console speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// English.
    #[default]
    En,
    /// Polish.
    Pl,
}

impl Lang {
    /// Guess from the environment, falling back to English.
    ///
    /// Checked in the order a user would expect to win: an explicit `SENTIN_LANG`, then the POSIX
    /// locale variables, then the Windows user locale. Anything unrecognised is English, because a
    /// console in a language the reader does not know is worse than one in the language everything
    /// else in this project uses.
    #[must_use]
    pub fn detect() -> Self {
        for var in ["SENTIN_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(var) {
                let value = value.to_ascii_lowercase();
                if value.starts_with("pl") {
                    return Lang::Pl;
                }
                if !value.is_empty() && var == "SENTIN_LANG" {
                    return Lang::En;
                }
            }
        }
        #[cfg(windows)]
        if windows_locale_is_polish() {
            return Lang::Pl;
        }
        Lang::En
    }

    /// The other language, for the switch in the window.
    #[must_use]
    pub fn other(self) -> Self {
        match self {
            Lang::En => Lang::Pl,
            Lang::Pl => Lang::En,
        }
    }

    /// What this language calls itself.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Pl => "Polski",
        }
    }

    /// A human name for a detector.
    #[must_use]
    pub fn detector_label(self, kind: DataKind) -> &'static str {
        match (self, kind) {
            (Lang::En, DataKind::Pesel) => "PESEL (national ID number)",
            (Lang::Pl, DataKind::Pesel) => "PESEL",
            (Lang::En, DataKind::Nip) => "NIP (tax number, also as PL VAT)",
            (Lang::Pl, DataKind::Nip) => "NIP (także jako numer VAT PL)",
            (Lang::En, DataKind::VatEu) => "EU VAT number (other member states)",
            (Lang::Pl, DataKind::VatEu) => "Numer VAT UE (inne kraje)",
            (Lang::En, DataKind::Regon) => "REGON (business registry number)",
            (Lang::Pl, DataKind::Regon) => "REGON",
            (Lang::En, DataKind::Iban) => "Bank account number (IBAN)",
            (Lang::Pl, DataKind::Iban) => "Numer konta bankowego (IBAN)",
            (Lang::En, DataKind::PaymentCard) => "Payment card number",
            (Lang::Pl, DataKind::PaymentCard) => "Numer karty płatniczej",
            (Lang::En, DataKind::Email) => "Email address",
            (Lang::Pl, DataKind::Email) => "Adres e-mail",
            (Lang::En, DataKind::PhonePl) => "Polish telephone number",
            (Lang::Pl, DataKind::PhonePl) => "Polski numer telefonu",
            (Lang::En, DataKind::Person) => "Person's name",
            (Lang::Pl, DataKind::Person) => "Imię i nazwisko",
            (Lang::En, DataKind::Organization) => "Company or organisation",
            (Lang::Pl, DataKind::Organization) => "Firma lub organizacja",
            (Lang::En, DataKind::Location) => "Place name",
            (Lang::Pl, DataKind::Location) => "Nazwa miejscowości",
        }
    }

    /// What a mode does, said as an outcome rather than as a setting.
    #[must_use]
    pub fn mode_label(self, mode: Decision) -> &'static str {
        match (self, mode) {
            (Lang::En, Decision::Observed) => "Record only",
            (Lang::Pl, Decision::Observed) => "Tylko zapisz",
            (Lang::En, Decision::Advised) => "Warn",
            (Lang::Pl, Decision::Advised) => "Ostrzegaj",
            (Lang::En, Decision::Masked) => "Hide before sending",
            (Lang::Pl, Decision::Masked) => "Ukryj przed wysłaniem",
            (Lang::En, Decision::Blocked) => "Refuse the request",
            (Lang::Pl, Decision::Blocked) => "Odrzuć żądanie",
        }
    }

    /// The longer explanation shown under a mode.
    #[must_use]
    pub fn mode_help(self, mode: Decision) -> &'static str {
        match (self, mode) {
            (Lang::En, Decision::Observed) => {
                "The identifier is written to the audit trail and sent onwards unchanged. Nobody is \
                 told."
            }
            (Lang::Pl, Decision::Observed) => {
                "Identyfikator trafia do dziennika i jedzie dalej bez zmian. Nikt nie jest o tym \
                 informowany."
            }
            (Lang::En, Decision::Advised) => {
                "The identifier is sent onwards, and the finding is reported so somebody can see it \
                 happened."
            }
            (Lang::Pl, Decision::Advised) => {
                "Identyfikator jedzie dalej, ale znalezisko jest zgłoszone, więc widać, że do tego \
                 doszło."
            }
            (Lang::En, Decision::Masked) => {
                "The identifier is replaced before the request leaves this machine. The model sees \
                 a placeholder."
            }
            (Lang::Pl, Decision::Masked) => {
                "Identyfikator jest podmieniony, zanim żądanie opuści ten komputer. Model widzi \
                 tylko znacznik."
            }
            (Lang::En, Decision::Blocked) => {
                "The request is refused and never sent. Only identifiers with a checksum can do \
                 this, because refusing somebody's work on a guess is worse than the leak."
            }
            (Lang::Pl, Decision::Blocked) => {
                "Żądanie jest odrzucone i nie zostaje wysłane. Tak mogą działać tylko identyfikatory \
                 z sumą kontrolną - odrzucenie czyjejś pracy na podstawie domysłu szkodzi bardziej \
                 niż sam wyciek."
            }
        }
    }
}

/// Generate one accessor per string, so a missing translation cannot compile.
macro_rules! strings {
    ($($name:ident => $en:literal , $pl:literal ;)*) => {
        impl Lang {
            $(
                #[doc = concat!("Interface string: \"", $en, "\".")]
                #[must_use]
                pub fn $name(self) -> &'static str {
                    match self { Lang::En => $en, Lang::Pl => $pl }
                }
            )*
        }
    };
}

strings! {
    window_title => "Sentin-NPU console", "Sentin-NPU - konsola";

    tab_protection => "Protection", "Ochrona";
    tab_report => "Report", "Raport";
    tab_status => "Status", "Stan";

    protection_intro =>
        "Choose what happens to each kind of identifier before a request leaves this machine. A \
         detector nobody sets is only recorded - it is found, written to the audit trail, and sent \
         onwards anyway.",
        "Wybierz, co ma się stać z każdym rodzajem danych, zanim żądanie opuści ten komputer. \
         Detektor, którego nikt nie ustawił, jest tylko zapisywany - zostaje znaleziony, trafia do \
         dziennika i mimo to jedzie dalej.";

    group_checksum => "Verified by a checksum", "Potwierdzone sumą kontrolną";
    group_pattern => "Recognised by shape only", "Rozpoznawane tylko po kształcie";
    group_model => "Found by the language model", "Znajdowane przez model językowy";

    unset_note => "not set - only recorded", "nieustawione - tylko zapisywane";

    apply => "Save and apply", "Zapisz i zastosuj";
    revert => "Discard changes", "Odrzuć zmiany";
    pending_none => "No changes to save.", "Brak zmian do zapisania.";
    saved_ok => "Saved. The gateway is running the new settings.",
        "Zapisano. Bramka pracuje na nowych ustawieniach.";
    saved_no_restart =>
        "Saved, but the gateway could not be restarted, so it is still running the old settings.",
        "Zapisano, ale nie udało się zrestartować bramki - nadal pracuje na starych ustawieniach.";
    backup_note => "Previous settings kept as", "Poprzednie ustawienia zachowane jako";

    audit_section => "Audit trail", "Dziennik zdarzeń";
    audit_enabled => "Write an audit trail", "Zapisuj dziennik zdarzeń";
    audit_path => "File", "Plik";
    audit_note =>
        "The audit trail records that an identifier was found - never the identifier itself. It is \
         what the report below is built from.",
        "Dziennik zapisuje, że znaleziono identyfikator - nigdy samego identyfikatora. To z niego \
         powstaje raport poniżej.";
    syslog_enabled => "Also send events to a SIEM (CEF over syslog)",
        "Wysyłaj zdarzenia również do SIEM (CEF przez syslog)";
    syslog_address => "SIEM address", "Adres SIEM";

    report_intro =>
        "Build a single file summarising what the gateway found. Useful where there is no SIEM to \
         answer the same question.",
        "Zbuduj jeden plik podsumowujący to, co bramka znalazła. Przydatne tam, gdzie nie ma SIEM, \
         który odpowiedziałby na to samo pytanie.";
    report_source => "Audit file", "Plik dziennika";
    report_build => "Build report", "Zbuduj raport";
    report_open => "Open report", "Otwórz raport";
    report_empty => "This audit file has no events yet.", "Ten dziennik nie ma jeszcze zdarzeń.";
    report_done => "Report written to", "Raport zapisany do";
    report_period => "Period", "Okres";
    report_events => "events", "zdarzeń";

    status_service => "Gateway service", "Usługa bramki";
    status_running => "running", "działa";
    status_stopped => "stopped", "zatrzymana";
    status_absent => "not installed", "niezainstalowana";
    status_unknown => "unknown", "nieznany";
    status_config => "Settings file", "Plik ustawień";
    status_log => "Log file", "Plik dziennika bramki";
    status_layer2 =>
        "Layer 2 (the language model) is what finds names, companies and places. When it is \
         missing, the gateway still runs - it just finds less, and says so only in its log.",
        "Warstwa 2 (model językowy) znajduje nazwiska, firmy i miejscowości. Gdy jej brakuje, \
         bramka działa dalej - po prostu znajduje mniej, i mówi o tym wyłącznie w swoim dzienniku.";
    status_layer2_ready => "Layer 2 is running on", "Warstwa 2 działa na";
    status_layer2_missing => "Layer 2 is NOT running - only the checksum detectors are active.",
        "Warstwa 2 NIE działa - aktywne są tylko detektory z sumą kontrolną.";
    status_layer2_silent => "The log does not say, which usually means the gateway has not started \
         since the log was last cleared.",
        "Dziennik nic o tym nie mówi - zwykle znaczy to, że bramka nie startowała od ostatniego \
         wyczyszczenia dziennika.";
    status_restart => "Restart the gateway", "Zrestartuj bramkę";
    status_elevate =>
        "Changing settings and restarting the service need administrator rights, which this console \
         was not started with. Elevating opens the same window again; anything changed here and not \
         saved is lost.",
        "Zmiana ustawień i restart usługi wymagają uprawnień administratora, a ta konsola została \
         uruchomiona bez nich. Podniesienie uprawnień otwiera to samo okno od nowa - niezapisane \
         zmiany przepadają.";
    elevate_button => "Restart as administrator", "Uruchom jako administrator";
    elevate_failed => "Windows did not start the console with administrator rights.",
        "Windows nie uruchomił konsoli z uprawnieniami administratora.";
}

/// Whether the Windows user locale is Polish.
///
/// `GetUserDefaultLocaleName` without binding to the Win32 API: the `LANG`-style variables are not
/// set on a typical Windows session, so the fallback is the locale the user picked for their
/// account, read from the registry through the same tool the rest of this crate uses for service
/// control.
#[cfg(windows)]
fn windows_locale_is_polish() -> bool {
    use std::process::Command;
    // `Get-Culture` is the documented way to ask, and PowerShell is already a hard requirement of
    // this project's Windows tooling.
    let Ok(output) = Command::new("powershell")
        .args(["-NoProfile", "-Command", "(Get-Culture).Name"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_ascii_lowercase()
        .starts_with("pl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_detector_and_mode_is_named_in_both_languages() {
        // The match arms are exhaustive, so this cannot fail to compile with a kind missing. What
        // it checks is the other half: that nobody filled a Polish arm with the English string to
        // make the compiler stop complaining.
        for lang in [Lang::En, Lang::Pl] {
            for kind in DataKind::ALL {
                assert!(!lang.detector_label(kind).is_empty());
            }
            for mode in [
                Decision::Observed,
                Decision::Advised,
                Decision::Masked,
                Decision::Blocked,
            ] {
                assert!(!lang.mode_label(mode).is_empty());
                assert!(!lang.mode_help(mode).is_empty());
            }
        }
        for kind in DataKind::ALL {
            assert_ne!(
                Lang::En.detector_label(kind),
                Lang::Pl.detector_label(kind),
                "{kind:?} carries the same text in both languages"
            );
        }
    }

    #[test]
    fn the_wording_stays_within_the_plain_hyphen_rule() {
        // The rule covers user-visible output, and this crate is nothing but user-visible output.
        // It was broken once already, in `sentin-bench`, and the Windows console rendered it as
        // mojibake.
        for lang in [Lang::En, Lang::Pl] {
            let mut all: Vec<&str> = vec![
                lang.window_title(),
                lang.protection_intro(),
                lang.report_intro(),
                lang.status_layer2(),
                lang.status_elevate(),
                lang.audit_note(),
            ];
            for kind in DataKind::ALL {
                all.push(lang.detector_label(kind));
            }
            for text in all {
                assert!(
                    !text.contains('\u{2014}') && !text.contains('\u{2013}'),
                    "long dash in: {text}"
                );
            }
        }
    }
}
