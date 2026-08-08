// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Latency harness for metrics M2a (proxy overhead) and M2c (streaming TTFT impact).
//!
//! Run with:
//!
//! ```text
//! cargo run --release -p sentin-proxy --bin sentin-bench
//! cargo run --release -p sentin-proxy --bin sentin-bench -- --json results.json
//! ```
//!
//! Everything runs against the in-process mock upstream, so the number reported is the gateway's
//! own cost rather than the internet's variance. The comparison that defines M2a is
//! *client → mock* against *client → gateway → mock* on the same machine, in the same process
//! tree, with the same payload.
//!
//! Per the project's benchmarking rules: several runs, the first discarded as warm-up, and both
//! median and p95 reported rather than a mean.

use std::time::{Duration, Instant};

use sentin_proxy::config::Config;
use sentin_proxy::mock::{self, StreamShape};
use sentin_proxy::AppState;
use serde_json::{json, Value};

const WARMUP: usize = 50;
const SAMPLES: usize = 500;
const STREAM_SAMPLES: usize = 25;

#[tokio::main]
async fn main() {
    let json_out = std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--json")
        .map(|w| w[1].clone());

    let (upstream, _received) = mock::spawn(StreamShape::default()).await;
    let client = reqwest::Client::new();

    println!("Sentin-NPU proxy latency harness");
    println!("  upstream (mock): {upstream}");
    println!("  samples: {SAMPLES} (plus {WARMUP} discarded warm-up), streams: {STREAM_SAMPLES}\n");

    let mut results = Vec::new();

    // ---- M2a: request-path overhead -------------------------------------------------------
    let payload = payload_of_roughly(1024);

    let direct = measure(
        &client,
        &format!("{upstream}/v1/chat/completions"),
        &payload,
        SAMPLES,
    )
    .await;

    let gateway_off = spawn_gateway(&upstream, "false", "passthrough", "observe").await;
    let off = measure(
        &client,
        &format!("{gateway_off}/openai/v1/chat/completions"),
        &payload,
        SAMPLES,
    )
    .await;

    let gateway_l1 = spawn_gateway(&upstream, "true", "passthrough", "mask").await;
    let l1 = measure(
        &client,
        &format!("{gateway_l1}/openai/v1/chat/completions"),
        &payload,
        SAMPLES,
    )
    .await;

    println!("M2a — request path, ~1 KB payload, non-streaming");
    println!(
        "  {:<34}{:>10}{:>10}",
        "configuration", "p50 (ms)", "p95 (ms)"
    );
    for (label, stats) in [
        ("direct to upstream (baseline)", &direct),
        ("via gateway, inspection off", &off),
        ("via gateway, L1 inspection on", &l1),
    ] {
        println!(
            "  {:<34}{:>10.3}{:>10.3}",
            label,
            stats.p50.as_secs_f64() * 1000.0,
            stats.p95.as_secs_f64() * 1000.0
        );
        results.push(json!({
            "metric": "M2a", "configuration": label,
            "p50_ms": stats.p50.as_secs_f64() * 1000.0,
            "p95_ms": stats.p95.as_secs_f64() * 1000.0,
        }));
    }

    let overhead_off_p95 = sub_ms(off.p95, direct.p95);
    let overhead_l1_p95 = sub_ms(l1.p95, direct.p95);
    println!(
        "\n  added p95, inspection off : {overhead_off_p95:+.3} ms   [{}]",
        verdict(overhead_off_p95 < 5.0)
    );
    println!("  added p95, L1 inspection  : {overhead_l1_p95:+.3} ms   (informational; M2b covers the full pipeline)");

    // ---- M2c: streaming, time to first token ----------------------------------------------
    println!("\nM2c — streaming, time to first byte reaching the client");
    println!(
        "  mock emits {} events, {} ms apart, sentence every 8th event",
        StreamShape::default().events,
        StreamShape::default().gap_ms
    );
    println!(
        "  {:<34}{:>12}{:>12}{:>12}",
        "strategy", "TTFT p50", "TTFT p95", "total p50"
    );

    let baseline_ttft = {
        let stats = measure_stream(&client, &format!("{upstream}/v1/chat/completions")).await;
        println!(
            "  {:<34}{:>12.1}{:>12.1}{:>12.1}",
            "direct to upstream (baseline)",
            stats.ttft_p50.as_secs_f64() * 1000.0,
            stats.ttft_p95.as_secs_f64() * 1000.0,
            stats.total_p50.as_secs_f64() * 1000.0
        );
        stats.ttft_p50
    };

    for strategy in ["passthrough", "buffer", "sliding_window"] {
        let gateway = spawn_gateway(&upstream, "true", strategy, "mask").await;
        let stats = measure_stream(&client, &format!("{gateway}/openai/v1/chat/completions")).await;
        let delta = sub_ms(stats.ttft_p50, baseline_ttft);
        println!(
            "  {:<34}{:>12.1}{:>12.1}{:>12.1}   ({delta:+.1} ms vs baseline)",
            format!("via gateway: {strategy}"),
            stats.ttft_p50.as_secs_f64() * 1000.0,
            stats.ttft_p95.as_secs_f64() * 1000.0,
            stats.total_p50.as_secs_f64() * 1000.0
        );
        results.push(json!({
            "metric": "M2c", "strategy": strategy,
            "ttft_p50_ms": stats.ttft_p50.as_secs_f64() * 1000.0,
            "ttft_p95_ms": stats.ttft_p95.as_secs_f64() * 1000.0,
            "total_p50_ms": stats.total_p50.as_secs_f64() * 1000.0,
            "ttft_delta_vs_baseline_ms": delta,
        }));
    }

    if let Some(path) = json_out {
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&results).unwrap_or_default(),
        )
        .unwrap_or_else(|err| eprintln!("could not write {path}: {err}"));
        println!("\nwrote {path}");
    }
}

fn verdict(ok: bool) -> &'static str {
    if ok {
        "PASS (< 5 ms)"
    } else {
        "FAIL (>= 5 ms)"
    }
}

fn sub_ms(a: Duration, b: Duration) -> f64 {
    a.as_secs_f64() * 1000.0 - b.as_secs_f64() * 1000.0
}

/// A request body of roughly `target` bytes, containing no detectable identifiers.
///
/// Kept clean on purpose: M2a is the cost of *being in the path*, so the payload must not also be
/// paying for masking and re-serialisation.
fn payload_of_roughly(target: usize) -> Value {
    let filler = "Prosze podsumowac raport kwartalny dla zespolu sprzedazy. ";
    let mut content = String::with_capacity(target + filler.len());
    while content.len() < target {
        content.push_str(filler);
    }
    json!({"model": "bench", "messages": [{"role": "user", "content": content}]})
}

async fn spawn_gateway(upstream: &str, inspect: &str, strategy: &str, mode: &str) -> String {
    let yaml = format!(
        "providers:\n  openai:\n    prefix: /openai\n    upstream: {upstream}\n\
         detectors:\n  pesel: {{ mode: {mode} }}\n  email: {{ mode: {mode} }}\n\
         inspect:\n  request: {inspect}\n  stream_strategy: {strategy}\n"
    );
    let config: Config = serde_yaml_ng::from_str(&yaml).expect("bench config parses");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind gateway");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = sentin_proxy::serve(listener, AppState::new(config)).await;
    });
    format!("http://{address}")
}

struct Stats {
    p50: Duration,
    p95: Duration,
}

async fn measure(client: &reqwest::Client, url: &str, body: &Value, samples: usize) -> Stats {
    for _ in 0..WARMUP {
        let _ = client.post(url).json(body).send().await;
    }
    let mut timings = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let response = client.post(url).json(body).send().await.expect("request");
        let _ = response.bytes().await;
        timings.push(started.elapsed());
    }
    Stats {
        p50: percentile(&mut timings, 0.50),
        p95: percentile(&mut timings, 0.95),
    }
}

struct StreamStats {
    ttft_p50: Duration,
    ttft_p95: Duration,
    total_p50: Duration,
}

async fn measure_stream(client: &reqwest::Client, url: &str) -> StreamStats {
    use futures_util::StreamExt;

    let body = json!({"stream": true, "messages": [{"role": "user", "content": "opowiedz"}]});

    // One discarded warm-up run, as the benchmarking rules require.
    if let Ok(response) = client.post(url).json(&body).send().await {
        let _ = response.bytes().await;
    }

    let mut ttfts = Vec::with_capacity(STREAM_SAMPLES);
    let mut totals = Vec::with_capacity(STREAM_SAMPLES);
    for _ in 0..STREAM_SAMPLES {
        let started = Instant::now();
        let response = client.post(url).json(&body).send().await.expect("request");
        let mut stream = response.bytes_stream();
        let mut first: Option<Duration> = None;
        while let Some(chunk) = stream.next().await {
            if chunk.is_ok() && first.is_none() {
                first = Some(started.elapsed());
            }
        }
        ttfts.push(first.unwrap_or_else(|| started.elapsed()));
        totals.push(started.elapsed());
    }

    StreamStats {
        ttft_p50: percentile(&mut ttfts, 0.50),
        ttft_p95: percentile(&mut ttfts, 0.95),
        total_p50: percentile(&mut totals, 0.50),
    }
}

fn percentile(samples: &mut [Duration], quantile: f64) -> Duration {
    samples.sort_unstable();
    if samples.is_empty() {
        return Duration::ZERO;
    }
    // Nearest-rank: with 500 samples the interpolation choice is noise, and this one is easy to
    // reproduce in another language when someone checks the article's numbers.
    let rank = ((samples.len() as f64) * quantile).ceil() as usize;
    samples[rank.clamp(1, samples.len()) - 1]
}
