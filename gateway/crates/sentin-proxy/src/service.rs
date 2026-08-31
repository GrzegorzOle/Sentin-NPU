// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Running the gateway as a Windows service.
//!
//! A privacy gateway that has to be started by hand is a privacy gateway that is sometimes not
//! running, and on Windows the clients most likely to route through it - a chat UI, an agent, an
//! IDE plugin - start with the desktop rather than with a terminal. So the shipped binary is also
//! a service binary.
//!
//! This is implemented against the Windows service API rather than by wrapping the executable in
//! something like NSSM or WinSW. A wrapper is another program to ship, license and explain, and it
//! reports its own health rather than the gateway's: the service control manager would see the
//! wrapper running while the gateway inside it had exited. Here, `Stopped` means stopped.
//!
//! The one behaviour worth knowing: the service **stops gracefully**. On `SERVICE_CONTROL_STOP` it
//! stops accepting connections and lets in-flight requests finish, because the requests passing
//! through are somebody's real work and cutting them off to shave a second off a restart is a poor
//! trade.

// `define_windows_service!` generates an `extern "system"` entry point, which the workspace lint
// flags. The unsafety belongs to the macro's FFI boundary, not to code written here.
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

/// The name the service control manager knows it by. Also what `sc query` takes.
pub const SERVICE_NAME: &str = "SentinNPU";

/// What an operator sees in services.msc.
pub const DISPLAY_NAME: &str = "Sentin-NPU privacy gateway";

const DESCRIPTION: &str = "Inspects LLM traffic on this machine and masks identifiers before they \
                           leave it. Listens on the configured port; see config.yaml.";

/// Service type for a process that hosts one service of its own.
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

windows_service::define_windows_service!(ffi_service_main, service_main);

/// Hand control to the service control manager. Only returns when the service has stopped.
///
/// # Errors
/// Fails when the process was not started by the service control manager, which is the usual
/// result of running `--service` by hand from a shell.
pub fn run() -> Result<(), windows_service::Error> {
    windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

/// The configuration path the service should use.
///
/// Taken from the process command line rather than from the arguments the control manager passes
/// at start time: the path belongs to the installation, and an operator who clicks Start in
/// services.msc passes nothing. It is `binPath` that carries it, and `binPath` is what the
/// installer wrote.
fn config_path_from_command_line() -> String {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|arg| arg == "--service")
        .and_then(|index| args.get(index + 1))
        .filter(|arg| !arg.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| {
            // Beside the executable, which is where the installer puts it.
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("config.yaml")))
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "config.yaml".to_string())
        })
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(err) = run_service() {
        // Nothing to print to: a service has no console. The event log would need another
        // dependency, so the failure is reported through the exit code the control manager shows.
        tracing::error!(error = %err, "service failed");
    }
}

fn run_service() -> Result<(), Box<dyn std::error::Error>> {
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

    let handler = move |control| match control {
        // Interrogate is the control manager asking for the current status; answering it is
        // mandatory and means nothing else.
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = shutdown_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        _ => ServiceControlHandlerResult::NotImplemented,
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, handler)?;

    let running = ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    };
    status_handle.set_service_status(running)?;

    let config_path = config_path_from_command_line();
    let result = serve_until_stopped(&config_path, shutdown_rx);

    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        // A configuration the gateway cannot load is reported as a failure rather than a clean
        // stop, so services.msc shows something went wrong instead of a service that quietly is
        // not there.
        exit_code: ServiceExitCode::Win32(u32::from(result.is_err())),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    result
}

fn serve_until_stopped(
    config_path: &str,
    shutdown_rx: mpsc::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = crate::config::Config::load(config_path)?;
    let address = format!("{}:{}", config.listen.host, config.listen.port);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&address).await?;
        tracing::info!(%address, config = %config_path, "sentin-gateway service listening");

        let state = crate::AppState::with_inference(config);
        // The blocking receive lives on a dedicated thread: the control handler runs on a thread
        // the control manager owns, and blocking it would make the service look hung.
        let shutdown = async move {
            let _ = tokio::task::spawn_blocking(move || shutdown_rx.recv()).await;
            tracing::info!("stop requested; finishing in-flight requests");
        };
        crate::serve_with_shutdown(listener, state, shutdown).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

/// Register the service with the control manager.
///
/// `config_path` is written into the service's `binPath`, so an operator can see which
/// configuration a service uses with `sc qc SentinNPU` instead of guessing.
///
/// # Errors
/// Fails without administrator rights, or when a service of this name already exists.
pub fn install(config_path: &std::path::Path) -> Result<(), windows_service::Error> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;

    let executable: PathBuf = std::env::current_exe().map_err(windows_service::Error::Winapi)?;
    let service = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: SERVICE_TYPE,
        // Automatic, not delayed: something starting with the desktop may well make its first
        // request before a delayed service is up, and a request that misses the gateway is
        // precisely the request nobody inspected.
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: executable,
        launch_arguments: vec![
            OsString::from("--service"),
            OsString::from(config_path.as_os_str()),
        ],
        dependencies: vec![],
        // LocalSystem. The gateway needs to bind a port, read its model and write its audit file,
        // and nothing else; a dedicated account would be better practice and is a roadmap item.
        account_name: None,
        account_password: None,
    };

    let handle = manager.create_service(&service, ServiceAccess::CHANGE_CONFIG)?;
    handle.set_description(DESCRIPTION)?;
    Ok(())
}

/// Stop the service if it is running, then remove it.
///
/// # Errors
/// Fails without administrator rights, or when no such service exists.
pub fn uninstall() -> Result<(), windows_service::Error> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    )?;

    if service.query_status()?.current_state != ServiceState::Stopped {
        service.stop()?;
        // Deleting a service that is still stopping leaves it marked for deletion until every
        // handle closes, which reads to an operator as "uninstall did nothing".
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(500));
            if service.query_status()?.current_state == ServiceState::Stopped {
                break;
            }
        }
    }

    service.delete()?;
    Ok(())
}
