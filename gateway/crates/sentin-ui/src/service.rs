// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Asking the operating system what the gateway is doing, and restarting it.
//!
//! Saving a settings file changes nothing on its own: the gateway reads its configuration once, at
//! startup. A console that wrote the file and stopped there would leave the user exactly where they
//! started - believing a change took effect when it did not - which is the failure this whole
//! project keeps finding in other forms.
//!
//! The service is queried through the service control manager rather than by parsing `sc.exe`, for
//! a reason found on the machine this was written on: its shell answers in Polish, so the text of
//! any command output is a localisation setting away from being unparseable. A status code is not.
//!
//! **Running is not inspecting.** [`State::Running`] means the process is alive, and this project
//! has shipped a running gateway that was quietly inspecting half of what it claimed. That is why
//! the console pairs this with the gateway's own log line about layer 2 rather than presenting a
//! green light on its own.

use std::path::{Path, PathBuf};

/// What the service control manager says about the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The service exists and is running. This says nothing about what it is inspecting.
    Running,
    /// The service exists and is not running.
    Stopped,
    /// No service by that name is registered - a source build started by hand, most likely.
    NotInstalled,
    /// The service could not be queried, usually for want of rights.
    Unknown,
}

/// What the gateway's own log says about layer 2 on its most recent start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layer2 {
    /// Layer 2 came up, on the named device.
    Ready(String),
    /// Layer 2 did not come up, so only the deterministic detectors are running.
    Unavailable,
    /// The log does not cover a start.
    Silent,
}

/// Query the gateway service.
#[must_use]
pub fn state() -> State {
    #[cfg(windows)]
    {
        windows::state()
    }
    #[cfg(not(windows))]
    {
        unix::state()
    }
}

/// Stop and start the gateway so it rereads its configuration.
///
/// # Errors
/// If the service cannot be controlled, which on Windows is usually a console that was not started
/// with administrator rights.
pub fn restart() -> Result<(), String> {
    #[cfg(windows)]
    {
        windows::restart()
    }
    #[cfg(not(windows))]
    {
        unix::restart()
    }
}

/// Where the service writes its log, given the configuration it was started with.
#[must_use]
pub fn log_path(config: &Path) -> PathBuf {
    config.with_file_name("sentin-gateway.log")
}

/// Read the last thing the gateway said about layer 2.
///
/// Scans backwards, and that is the whole point: the gateway appends across restarts, so the first
/// matching line in the file can easily be last week's success, reported about a start that has
/// just failed. The installer made exactly this mistake and passed an installation that had not
/// worked.
#[must_use]
pub fn layer2_from_log(log: &Path) -> Layer2 {
    let Ok(text) = std::fs::read_to_string(log) else {
        return Layer2::Silent;
    };
    for line in text.lines().rev() {
        if let Some(at) = line.find("layer 2 ready") {
            let device = line[at..]
                .split("device=")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .unwrap_or("?")
                .to_string();
            return Layer2::Ready(device);
        }
        if line.contains("layer 2 unavailable") {
            return Layer2::Unavailable;
        }
    }
    Layer2::Silent
}

/// Whether this process can change the installed configuration and control the service.
///
/// Checked by trying, not by asking who the user is: group membership does not answer the question
/// on a machine with UAC, where an administrator's ordinary process holds a filtered token and
/// cannot write to `Program Files` at all.
#[must_use]
pub fn can_administer(config: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new().append(true).open(config) else {
        return false;
    };
    drop(file);
    true
}

/// Start this console again, with administrator rights, on the same configuration.
///
/// The console is a Start Menu program that ordinary users are meant to open, so it does not
/// declare `requireAdministrator` in a manifest: that would put a UAC prompt in front of somebody
/// who only wanted to read a report. The rights are asked for at the point they are needed, which
/// is the moment a setting is about to be written.
///
/// # Errors
/// If the elevated process could not be started, which includes the user declining the prompt.
pub fn relaunch_elevated(config: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows::relaunch_elevated(config)
    }
    #[cfg(not(windows))]
    {
        let _ = config;
        Err("run this console as the user that owns the configuration file".to_string())
    }
}

#[cfg(windows)]
mod windows {
    use super::State;
    use std::path::Path;
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    const NAME: &str = sentin_proxy::service::SERVICE_NAME;

    pub(super) fn state() -> State {
        let Ok(manager) =
            ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        else {
            return State::Unknown;
        };
        match manager.open_service(NAME, ServiceAccess::QUERY_STATUS) {
            Ok(service) => match service.query_status() {
                Ok(status) if status.current_state == ServiceState::Running => State::Running,
                Ok(_) => State::Stopped,
                Err(_) => State::Unknown,
            },
            // 1060 is "the specified service does not exist", which is not an error worth showing
            // as one: a source build started by hand is a legitimate way to run this.
            Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(1060) => {
                State::NotInstalled
            }
            Err(_) => State::Unknown,
        }
    }

    pub(super) fn restart() -> Result<(), String> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .map_err(|e| format!("cannot reach the service control manager: {e}"))?;
        let service = manager
            .open_service(
                NAME,
                ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::START,
            )
            .map_err(|e| format!("cannot open the {NAME} service: {e}"))?;

        if service
            .query_status()
            .map(|s| s.current_state != ServiceState::Stopped)
            .unwrap_or(false)
        {
            service
                .stop()
                .map_err(|e| format!("cannot stop the service: {e}"))?;
            // The gateway finishes in-flight requests before exiting, so the handle is not free
            // the instant `stop` returns. Starting too early fails with "service is marked for
            // deletion" or simply refuses.
            for _ in 0..60 {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if service
                    .query_status()
                    .map(|s| s.current_state == ServiceState::Stopped)
                    .unwrap_or(false)
                {
                    break;
                }
            }
        }

        service
            .start::<&str>(&[])
            .map_err(|e| format!("cannot start the service: {e}"))?;

        // Report the state that was reached, not the fact that the call returned. A start that is
        // accepted and then fails immediately is the case worth catching, and it is the one a
        // caller would otherwise report as success.
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            match service.query_status().map(|s| s.current_state) {
                Ok(ServiceState::Running) => return Ok(()),
                Ok(ServiceState::Stopped) => {
                    return Err("the service started and stopped again; see its log".into())
                }
                _ => {}
            }
        }
        Err("the service did not reach a running state in 15 seconds".into())
    }

    pub(super) fn relaunch_elevated(config: &Path) -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        use std::process::Command;

        // `Start-Process -Verb RunAs` rather than `ShellExecuteW`, which is the same call one layer
        // down: this crate would have to declare `unsafe` for it, and the workspace treats that as
        // something to justify rather than to spend on a single shell verb.
        let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
        let script = format!(
            "Start-Process -FilePath {} -ArgumentList {} -Verb RunAs",
            quote(&exe.to_string_lossy()),
            quote(&config.to_string_lossy())
        );
        // CREATE_NO_WINDOW: without it the helper flashes a console window in front of the user,
        // which looks exactly like something crashing.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let status = Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|e| format!("cannot ask Windows to elevate: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err("the request for administrator rights was refused".to_string())
        }
    }

    /// A PowerShell single-quoted string, where the only escape is a doubled quote.
    fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }
}

#[cfg(not(windows))]
mod unix {
    use super::State;
    use std::process::Command;

    const UNIT: &str = "sentin-npu";

    pub(super) fn state() -> State {
        let Ok(output) = Command::new("systemctl")
            .args(["--user", "is-active", UNIT])
            .output()
        else {
            return State::Unknown;
        };
        // `is-active` answers on stdout with a stable set of words and does not localise them.
        match String::from_utf8_lossy(&output.stdout).trim() {
            "active" => State::Running,
            "inactive" | "failed" | "activating" | "deactivating" => State::Stopped,
            "unknown" | "" => State::NotInstalled,
            _ => State::Unknown,
        }
    }

    pub(super) fn restart() -> Result<(), String> {
        let output = Command::new("systemctl")
            .args(["--user", "restart", UNIT])
            .output()
            .map_err(|e| format!("cannot run systemctl: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_layer_two_line_wins() {
        // The gateway appends across restarts. Reading forwards would find the older success and
        // report a working layer 2 for a start that had just failed - the exact defect the Windows
        // installer shipped in v0.1.1.
        let dir = std::env::temp_dir().join("sentin-ui-log-order");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let log = dir.join("sentin-gateway.log");
        std::fs::write(
            &log,
            "2026-09-01T10:00:00Z INFO sentin_proxy: layer 2 ready device=CPU fell_back=false\n\
             2026-09-02T10:00:00Z WARN sentin_proxy: layer 2 unavailable; continuing with layer 1\n",
        )
        .expect("write");
        assert_eq!(layer2_from_log(&log), Layer2::Unavailable);

        std::fs::write(
            &log,
            "2026-09-01T10:00:00Z WARN sentin_proxy: layer 2 unavailable\n\
             2026-09-02T10:00:00Z INFO sentin_proxy: layer 2 ready device=NPU fell_back=false\n",
        )
        .expect("write");
        assert_eq!(layer2_from_log(&log), Layer2::Ready("NPU".into()));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_log_that_says_nothing_is_not_a_success() {
        let missing = std::env::temp_dir().join("sentin-ui-no-such-log.log");
        std::fs::remove_file(&missing).ok();
        assert_eq!(layer2_from_log(&missing), Layer2::Silent);
    }

    #[test]
    fn the_log_sits_beside_the_configuration() {
        let config = Path::new("C:/ProgramData/Sentin-NPU/config.yaml");
        assert!(log_path(config).ends_with("sentin-gateway.log"));
        assert_eq!(log_path(config).parent(), config.parent());
    }
}
