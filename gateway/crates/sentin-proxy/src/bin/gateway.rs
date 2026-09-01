// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! The `sentin-gateway` binary.

use std::process::ExitCode;

use sentin_proxy::config::Config;
use sentin_proxy::{serve, AppState};

#[tokio::main]
async fn main() -> ExitCode {
    // First, and before any inspection thread exists: a runtime shipped beside this executable has
    // to be on the library search path before OpenVINO is first asked for anything, and writing to
    // the environment is only defensible while nothing else is reading it.
    sentin_detect::ov::use_bundled_runtime();

    let args: Vec<String> = std::env::args().collect();

    // Under the service control manager there is no console, so the log goes to a file. The choice
    // has to be made here because a subscriber can be installed only once, and installing the
    // stdout one first is what made every service start silent.
    #[cfg(windows)]
    let started_by_scm = args.iter().any(|a| a == "--service");
    #[cfg(not(windows))]
    let started_by_scm = false;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        // Both targets: the library emits per-request lines as `sentin_proxy`, while this binary's
        // own startup message is `sentin_gateway`. Filtering on the library alone meant the gateway
        // came up completely silently, which reads as a failure to start.
        .unwrap_or_else(|_| "sentin_proxy=info,sentin_gateway=info".into());

    if started_by_scm {
        #[cfg(windows)]
        sentin_proxy::service::init_logging(filter);
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    // Windows service verbs. All three are no-ops elsewhere, and saying so is better than hiding
    // the flags: someone reading --help on Linux should see why they are absent.
    #[cfg(windows)]
    {
        let flag = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());

        if args.iter().any(|a| a == "--service") {
            // Started by the service control manager. Nothing is printed: there is no console.
            if let Err(err) = sentin_proxy::service::run() {
                eprintln!("sentin-gateway: not started by the service manager: {err}");
                return ExitCode::FAILURE;
            }
            return ExitCode::SUCCESS;
        }

        if args.iter().any(|a| a == "--install-service") {
            let config = flag("--install-service")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::current_exe()
                        .ok()
                        .and_then(|exe| exe.parent().map(|dir| dir.join("config.yaml")))
                        .unwrap_or_else(|| std::path::PathBuf::from("config.yaml"))
                });
            return match sentin_proxy::service::install(&config) {
                Ok(()) => {
                    println!(
                        "installed service {} using {}",
                        sentin_proxy::service::SERVICE_NAME,
                        config.display()
                    );
                    println!(
                        "start it with: sc start {}",
                        sentin_proxy::service::SERVICE_NAME
                    );
                    // Said here because it is the one thing an operator cannot guess, and the
                    // line to look for in it - "layer 2 ready" against "layer 2 unavailable" - is
                    // the difference between a gateway inspecting and a gateway pretending to.
                    println!(
                        "it logs to: {}",
                        sentin_proxy::service::log_path(&config).display()
                    );
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("sentin-gateway: cannot install the service: {err}");
                    eprintln!("this needs an elevated prompt");
                    ExitCode::FAILURE
                }
            };
        }

        if args.iter().any(|a| a == "--uninstall-service") {
            return match sentin_proxy::service::uninstall() {
                Ok(()) => {
                    println!("removed service {}", sentin_proxy::service::SERVICE_NAME);
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("sentin-gateway: cannot remove the service: {err}");
                    ExitCode::FAILURE
                }
            };
        }
    }

    // --doctor is the diagnostic path: report what this machine can actually do and exit. It is
    // deliberately available in the shipped binary rather than a dev-only tool, because the people
    // with Intel NPUs are the ones who need to run it.
    if args.iter().any(|a| a == "--doctor") {
        let flag = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
        let model = flag("--model").map(std::path::PathBuf::from);
        let report = sentin_proxy::doctor::run(model.as_deref());
        sentin_proxy::doctor::print(&report);

        if let Some(path) = flag("--json") {
            match serde_json::to_string_pretty(&report) {
                Ok(text) => match std::fs::write(&path, text) {
                    Ok(()) => println!("\nwrote {path}"),
                    Err(err) => eprintln!("could not write {path}: {err}"),
                },
                Err(err) => eprintln!("could not serialise report: {err}"),
            }
        }
        return ExitCode::SUCCESS;
    }

    let path = args
        .get(1)
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "config/default.yaml".to_string());

    let config = match Config::load(&path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("sentin-gateway: {err}");
            return ExitCode::FAILURE;
        }
    };

    let address = format!("{}:{}", config.listen.host, config.listen.port);
    let listener = match tokio::net::TcpListener::bind(&address).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("sentin-gateway: cannot bind {address}: {err}");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(
        %address,
        providers = config.providers.len(),
        strategy = ?config.inspect.stream_strategy,
        "sentin-gateway listening"
    );

    if let Err(err) = serve(listener, AppState::with_inference(config)).await {
        eprintln!("sentin-gateway: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
