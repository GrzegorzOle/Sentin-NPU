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
            json_out,
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

    // ---- M2b: full pipeline, layer 1 + layer 2 ---------------------------------------------
    // Only meaningful when the IR is present; skipping is reported rather than silently omitted.
    let model_dir = std::path::Path::new("models/herbert/int8/seq128");
    if model_dir.join("openvino_model.xml").exists() {
        let ner_payload = json!({"model": "bench", "messages": [{"role": "user",
            "content": "Klient Marek Nowak z Warszawy zlozyl wniosek o kredyt w Alterna Logistyka."}]});
        let gateway_l2 = spawn_gateway_with_model(
            &upstream,
            "true",
            "passthrough",
            "mask",
            "models/herbert/int8/seq128",
            "CPU",
        )
        .await;
        let l2 = measure(
            &client,
            &format!("{gateway_l2}/openai/v1/chat/completions"),
            &ner_payload,
            SAMPLES / 5,
        )
        .await;
        let l1_same_payload = measure(
            &client,
            &format!("{gateway_l1}/openai/v1/chat/completions"),
            &ner_payload,
            SAMPLES / 5,
        )
        .await;

        println!("\nM2b — full pipeline (L1+L2), device CPU, seq 128");
        println!(
            "  {:<34}{:>10}{:>10}",
            "configuration", "p50 (ms)", "p95 (ms)"
        );
        for (label, stats) in [
            ("via gateway, L1 only", &l1_same_payload),
            ("via gateway, L1 + L2 (NER)", &l2),
        ] {
            println!(
                "  {:<34}{:>10.3}{:>10.3}",
                label,
                stats.p50.as_secs_f64() * 1000.0,
                stats.p95.as_secs_f64() * 1000.0
            );
            results.push(json!({
                "metric": "M2b", "configuration": label,
                "p50_ms": stats.p50.as_secs_f64() * 1000.0,
                "p95_ms": stats.p95.as_secs_f64() * 1000.0,
            }));
        }
        let added_p95 = sub_ms(l2.p95, direct.p95);
        println!(
            "  added p95 vs direct: {added_p95:+.2} ms   [{}]",
            if added_p95 < 150.0 {
                "PASS (< 150 ms on CPU)"
            } else {
                "FAIL"
            }
        );
    } else {
        println!(
            "\nM2b — SKIPPED: no IR at models/herbert/int8/seq128 (run tools/prepare_model.py)"
        );
    }

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
    spawn_gateway_with_model(upstream, inspect, strategy, mode, "", "CPU").await
}

/// Same, but with layer 2 loaded from `model_dir` (empty disables it).
async fn spawn_gateway_with_model(
    upstream: &str,
    inspect: &str,
    strategy: &str,
    mode: &str,
    model_dir: &str,
    device: &str,
) -> String {
    let yaml = format!(
        "providers:\n  openai:\n    prefix: /openai\n    upstream: {upstream}\n\
         detectors:\n  pesel: {{ mode: {mode} }}\n  email: {{ mode: {mode} }}\n  \
           person: {{ mode: advise }}\n  organization: {{ mode: advise }}\n  \
           location: {{ mode: observe }}\n\
         inspect:\n  request: {inspect}\n  stream_strategy: {strategy}\n\
         inference:\n  device: {device}\n  model_dir: {model_dir}\n  timeout_ms: 5000\n"
    );
    let config: Config = serde_yaml_ng::from_str(&yaml).expect("bench config parses");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind gateway");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = sentin_proxy::serve(listener, AppState::with_inference(config)).await;
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
async fn energy_mode(
    client: &reqwest::Client,
    upstream: &str,
    rps: u64,
    duration_s: u64,
    json_out: Option<String>,
) {
    use sentin_proxy::energy::{Measurement, Reader};
    use sentin_proxy::fingerprint::Machine;

    println!("Sentin-NPU energy harness (M5b)\n");

    let reader = match Reader::new() {
        Ok(reader) => reader,
        Err(err) => {
            eprintln!("Cannot measure energy on this machine.\n\n{err}\n");
            eprintln!(
                "Domains present: {:?}",
                sentin_proxy::energy::domains()
                    .iter()
                    .map(|d| d.name.clone())
                    .collect::<Vec<_>>()
            );
            std::process::exit(2);
        }
    };

    let machine = Machine::detect(sentin_proxy::energy::BACKEND, reader.domain_names());
    let saturate = rps == 0;
    println!("  machine      : {}", machine.label());
    println!("  domains      : {:?}", machine.energy_domains);
    println!(
        "  load         : {}",
        if saturate {
            "saturation (as fast as the client can drive it)".to_string()
        } else {
            format!("{rps} rps, rate-limited")
        }
    );
    println!("  phase length : {duration_s} s");
    println!("  phase order  : idle, direct, gateway, direct, gateway, idle (interleaved)");

    let warnings = machine.comparability_warnings();
    if warnings.is_empty() {
        println!("  comparability: OK\n");
    } else {
        println!("\n  COMPARABILITY WARNINGS - the result is valid for this configuration only:");
        for warning in &warnings {
            println!("    * {warning}");
        }
        println!();
    }

    let payload = payload_of_roughly(1024);
    let gateway = spawn_gateway(upstream, "true", "passthrough", "mask").await;
    let direct_url = format!("{upstream}/v1/chat/completions");
    let gateway_url = format!("{gateway}/openai/v1/chat/completions");

    // Interleaved, and idle measured at both ends. A laptop's package power drifts with thermal
    // state, and a single idle phase taken first measures the machine cooling down from whatever
    // ran before -- which is how an earlier version of this harness produced workload phases that
    // drew *less* than "idle". The two idle runs bracket the drift and give a noise floor to
    // compare the signal against.
    let idle_first = run_phase(client, None, &payload, rps, duration_s, &reader).await;
    let direct_a = run_phase(
        client,
        Some(&direct_url),
        &payload,
        rps,
        duration_s,
        &reader,
    )
    .await;
    let gateway_a = run_phase(
        client,
        Some(&gateway_url),
        &payload,
        rps,
        duration_s,
        &reader,
    )
    .await;
    let direct_b = run_phase(
        client,
        Some(&direct_url),
        &payload,
        rps,
        duration_s,
        &reader,
    )
    .await;
    let gateway_b = run_phase(
        client,
        Some(&gateway_url),
        &payload,
        rps,
        duration_s,
        &reader,
    )
    .await;
    let idle_last = run_phase(client, None, &payload, rps, duration_s, &reader).await;

    println!(
        "  {:<12}{:>10}{:>12}{:>12}{:>14}{:>10}{:>10}{:>12}",
        "domain",
        "idle (W)",
        "noise (W)",
        "direct (W)",
        "gateway (W)",
        "mJ/req",
        "mJ/req",
        "overhead"
    );
    println!(
        "  {:<12}{:>10}{:>12}{:>12}{:>14}{:>10}{:>10}{:>12}",
        "", "", "", "", "", "direct", "gateway", "mJ/req"
    );

    let mut rows = Vec::new();
    for phase in &gateway_a.measurements {
        let name = &phase.domain;
        let watts = |set: &Phase| -> f64 {
            set.measurements
                .iter()
                .find(|m| &m.domain == name)
                .map_or(0.0, Measurement::watts)
        };

        let idle_a_w = watts(&idle_first);
        let idle_b_w = watts(&idle_last);
        // Drift between two identical idle phases is the smallest difference this setup can
        // honestly resolve. Anything below it is not a measurement, it is weather.
        let noise_w = (idle_a_w - idle_b_w).abs();

        let direct_w = (watts(&direct_a) + watts(&direct_b)) / 2.0;
        let gateway_w = (watts(&gateway_a) + watts(&gateway_b)) / 2.0;
        let idle_w = (idle_a_w + idle_b_w) / 2.0;

        let direct_reqs = (direct_a.requests + direct_b.requests) as f64;
        let gateway_reqs = (gateway_a.requests + gateway_b.requests) as f64;
        let seconds = duration_s as f64 * 2.0;

        // Energy per request, not power. Under saturation the gateway is the bottleneck, so it
        // completes far fewer requests than the direct path and therefore draws *less* power
        // while costing *more* per request. Comparing watts here would invert the conclusion.
        let per_request_mj = |watts: f64, requests: f64| -> f64 {
            if requests > 0.0 {
                (watts - idle_w).max(0.0) * seconds * 1000.0 / requests
            } else {
                0.0
            }
        };
        let direct_mj = per_request_mj(direct_w, direct_reqs);
        let gateway_mj = per_request_mj(gateway_w, gateway_reqs);
        let overhead_mj = gateway_mj - direct_mj;

        // Resolvable only if the active power of both phases clears the platform's own drift.
        let resolved = (direct_w - idle_w).abs() > noise_w && (gateway_w - idle_w).abs() > noise_w;
        println!(
            "  {name:<12}{idle_w:>10.2}{noise_w:>12.2}{direct_w:>12.2}{gateway_w:>14.2}{:>10.4}{:>10.4}{:>12}",
            direct_mj,
            gateway_mj,
            if resolved {
                format!("{overhead_mj:+.4}")
            } else {
                "below noise".to_string()
            }
        );

        rows.push(json!({
            "domain": name,
            "idle_first_w": idle_a_w,
            "idle_last_w": idle_b_w,
            "idle_mean_w": idle_w,
            "noise_floor_w": noise_w,
            "direct_w": direct_w,
            "gateway_w": gateway_w,
            "direct_mj_per_request": direct_mj,
            "gateway_mj_per_request": gateway_mj,
            "overhead_mj_per_request": overhead_mj,
            "resolved_above_noise": resolved,
            "direct_requests": direct_reqs,
            "gateway_requests": gateway_reqs,
        }));
    }

    println!(
        "\n  requests completed: direct {}, gateway {}",
        direct_a.requests + direct_b.requests,
        gateway_a.requests + gateway_b.requests
    );
    if saturate {
        println!(
            "  At saturation the request counts differ by design; compare energy per request,"
        );
        println!("  not total energy.");
    }
    println!("  'below noise' means the gateway's draw could not be separated from the platform's");
    println!("  own drift -- an upper bound, not a zero. Raise the rate or lengthen the phases.");
    println!("\n  Caveat: package RAPL covers the whole SoC. On an Intel Core Ultra that includes");
    println!("  the NPU, with no separate domain, so NPU energy must be obtained by differencing");
    println!("  workloads rather than read directly. See docs/benchmarks.md.");

    if let Some(path) = json_out {
        // The fingerprint says whose result this is. Reports are per-machine on purpose: the
        // question is what the gateway costs on *this* hardware, not how two CPUs compare.
        let report = json!({
            "metric": "M5b",
            "machine": machine,
            "comparability_warnings": warnings,
            "load": if saturate { "saturation".to_string() } else { format!("{rps} rps") },
            "phase_duration_s": duration_s,
            "domains": rows,
        });
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        )
        .unwrap_or_else(|err| eprintln!("could not write {path}: {err}"));
        println!("\n  wrote {path}");
    }
}

/// One measured phase: energy per domain plus how many requests actually completed.
struct Phase {
    measurements: Vec<sentin_proxy::energy::Measurement>,
    requests: u64,
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
) -> Phase {
    use sentin_proxy::energy::{elapsed, Measurement};

    // Let the previous phase's heat and turbo state settle before sampling.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let start = reader.sample();
    let mut requests = 0u64;
    match url {
        None => tokio::time::sleep(Duration::from_secs(duration_s)).await,
        Some(url) => {
            let deadline = Instant::now() + Duration::from_secs(duration_s);
            // rps == 0 means saturate: at a realistic rate the gateway's duty cycle is a fraction
            // of a percent, far below what a laptop's package power can resolve.
            let mut ticker = (rps > 0).then(|| {
                let mut t = tokio::time::interval(Duration::from_secs_f64(1.0 / rps as f64));
                t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                t
            });
            while Instant::now() < deadline {
                if let Some(ticker) = ticker.as_mut() {
                    ticker.tick().await;
                }
                if let Ok(response) = client.post(url).json(payload).send().await {
                    let _ = response.bytes().await;
                    requests += 1;
                }
            }
        }
    }
    let end = reader.sample();

    let duration = elapsed(&start, &end);
    Phase {
        measurements: reader
            .delta_uj(&start, &end)
            .into_iter()
            .map(|(domain, uj)| Measurement {
                domain,
                energy_j: uj as f64 / 1_000_000.0,
                duration,
            })
            .collect(),
        requests,
    }
}
