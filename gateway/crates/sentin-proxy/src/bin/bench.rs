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
    let args: Vec<String> = std::env::args().collect();
    let json_out = args
        .windows(2)
        .find(|w| w[0] == "--json")
        .map(|w| w[1].clone());
    let flag = |name: &str, default: u64| -> u64 {
        args.windows(2)
            .find(|w| w[0] == name)
            .and_then(|w| w[1].parse().ok())
            .unwrap_or(default)
    };

    let (upstream, _received) = mock::spawn(StreamShape::default()).await;
    let client = reqwest::Client::new();

    if args.iter().any(|a| a == "--energy") {
        energy_mode(
            &client,
            &upstream,
            flag("--rps", 10),
            flag("--duration", 30),
        )
        .await;
        return;
    }

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

// ---------------------------------------------------------------------------------------------
// M5b — energy overhead of having the gateway in the path
// ---------------------------------------------------------------------------------------------

/// Measure package energy for three phases at a fixed request rate, and report the difference.
///
/// A fixed rate rather than saturation is what makes the number meaningful for a report: real
/// deployments run at some request rate and want to know the cost of that rate, whereas a
/// saturation test measures how fast the machine can spin, which nobody deploys.
///
/// The three phases are idle, direct-to-upstream, and via-gateway. Idle is subtracted from both
/// workloads, and the *difference between the two workloads* is the gateway's own cost — anything
/// common to both (the mock, the HTTP client, the OS) cancels out.
async fn energy_mode(client: &reqwest::Client, upstream: &str, rps: u64, duration_s: u64) {
    use sentin_proxy::energy::{Measurement, Reader};

    println!("Sentin-NPU energy harness (M5b)\n");

    let reader = match Reader::new() {
        Ok(reader) => reader,
        Err(err) => {
            eprintln!("Cannot read RAPL counters.\n\n{err}\n");
            eprintln!(
                "Domains present on this machine: {:?}",
                sentin_proxy::energy::domains()
                    .iter()
                    .map(|d| d.name.clone())
                    .collect::<Vec<_>>()
            );
            std::process::exit(2);
        }
    };

    println!("  RAPL domains : {:?}", reader.domain_names());
    println!("  rate         : {rps} rps");
    println!("  phase length : {duration_s} s each (idle, direct, gateway)\n");

    let payload = payload_of_roughly(1024);
    let gateway = spawn_gateway(upstream, "true", "passthrough", "mask").await;

    // Phase 1 — idle. Nothing but the harness itself, so the baseline includes the mock sitting
    // idle exactly as it does during the workload phases.
    let idle = run_phase(client, None, &payload, rps, duration_s, &reader).await;
    let idle_watts: Vec<(String, f64)> =
        idle.iter().map(|m| (m.domain.clone(), m.watts())).collect();

    let direct = run_phase(
        client,
        Some(&format!("{upstream}/v1/chat/completions")),
        &payload,
        rps,
        duration_s,
        &reader,
    )
    .await;

    let via = run_phase(
        client,
        Some(&format!("{gateway}/openai/v1/chat/completions")),
        &payload,
        rps,
        duration_s,
        &reader,
    )
    .await;

    let requests = (rps * duration_s) as f64;
    println!(
        "  {:<12}{:>12}{:>12}{:>12}{:>16}",
        "domain", "idle (W)", "direct (W)", "gateway (W)", "overhead"
    );

    for measurement in &via {
        let name = &measurement.domain;
        let idle_w = idle_watts
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0.0, |(_, w)| *w);
        let direct_w = direct
            .iter()
            .find(|m| &m.domain == name)
            .map_or(0.0, Measurement::watts);
        let gateway_w = measurement.watts();

        let direct_active = direct
            .iter()
            .find(|m| &m.domain == name)
            .map_or(0.0, |m| m.active_energy_j(idle_w));
        let gateway_active = measurement.active_energy_j(idle_w);
        let overhead_mj_per_request = (gateway_active - direct_active) * 1000.0 / requests;

        println!(
            "  {name:<12}{idle_w:>12.2}{direct_w:>12.2}{gateway_w:>12.2}{overhead_mj_per_request:>13.3} mJ/req"
        );
    }

    println!(
        "\n  Requests per phase: {requests:.0}. Overhead is (gateway - direct) after removing idle,"
    );
    println!("  so the mock upstream, the HTTP client and the OS cancel out of the figure.");
    println!(
        "  Mean added power at {rps} rps: {:.3} W\n",
        via.first()
            .zip(direct.first())
            .map(|(g, d)| g.watts() - d.watts())
            .unwrap_or(0.0)
    );
    println!("  Caveat: package RAPL includes every block on the SoC. On an Intel Core Ultra that");
    println!("  means the NPU too, with no separate domain — NPU energy has to be obtained by");
    println!("  differencing workloads, never read directly. See docs/benchmarks.md.");
}

/// Drive `url` at a fixed rate for `duration_s`, sampling energy across the interval.
/// `None` runs the idle phase: same duration, no requests.
async fn run_phase(
    client: &reqwest::Client,
    url: Option<&str>,
    payload: &Value,
    rps: u64,
    duration_s: u64,
    reader: &sentin_proxy::energy::Reader,
) -> Vec<sentin_proxy::energy::Measurement> {
    use sentin_proxy::energy::{elapsed, Measurement};

    // Let the previous phase's heat and turbo state settle before sampling.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let start = reader.sample();
    match url {
        None => tokio::time::sleep(Duration::from_secs(duration_s)).await,
        Some(url) => {
            let period = Duration::from_secs_f64(1.0 / rps as f64);
            let mut ticker = tokio::time::interval(period);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let deadline = Instant::now() + Duration::from_secs(duration_s);
            while Instant::now() < deadline {
                ticker.tick().await;
                if let Ok(response) = client.post(url).json(payload).send().await {
                    let _ = response.bytes().await;
                }
            }
        }
    }
    let end = reader.sample();

    let duration = elapsed(&start, &end);
    reader
        .delta_uj(&start, &end)
        .into_iter()
        .map(|(domain, uj)| Measurement {
            domain,
            energy_j: uj as f64 / 1_000_000.0,
            duration,
        })
        .collect()
}
