// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! The `sentin-gateway` binary.

use std::process::ExitCode;

use sentin_proxy::config::Config;
use sentin_proxy::{serve, AppState};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // Both targets: the library emits per-request lines as `sentin_proxy`, while this
                // binary's own startup message is `sentin_gateway`. Filtering on the library alone
                // meant the gateway came up completely silently, which reads as a failure to start.
                .unwrap_or_else(|_| "sentin_proxy=info,sentin_gateway=info".into()),
        )
        .init();

    let path = std::env::args()
        .nth(1)
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

    if let Err(err) = serve(listener, AppState::new(config)).await {
        eprintln!("sentin-gateway: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
