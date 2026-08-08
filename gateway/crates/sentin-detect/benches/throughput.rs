// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Metric M1: layer-1 throughput, threshold > 100 MB/s.
//!
//! This cost is paid synchronously on every proxied request, so it is a latency budget rather
//! than a curiosity. Three shapes are measured because they stress different paths: prose with no
//! identifiers (the common case, dominated by the boundary scan), prose with realistic identifier
//! density (the case that also pays for checksum validation), and digit-heavy text (the adversarial
//! case, where every token is a candidate).
//!
//! Run with:  cargo bench -p sentin-detect

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use sentin_detect::{detect, testdata};

/// Ordinary business prose containing no identifiers at all.
fn clean_text(target_bytes: usize) -> String {
    const PARAGRAPH: &str = "Zamówienie zostało potwierdzone i przekazane do realizacji. \
        Magazyn centralny wyśle przesyłkę w ciągu dwóch dni roboczych. \
        Faktura trafi do systemu księgowego po zakończeniu kompletacji. \
        The shipment was confirmed and handed over to the carrier this morning. ";
    PARAGRAPH.repeat(target_bytes / PARAGRAPH.len() + 1)
}

/// Prose with identifiers scattered through it, roughly one per 200 bytes.
fn text_with_identifiers(target_bytes: usize) -> String {
    let identifiers = [
        testdata::pesel(1944, 5, 14, 135),
        testdata::nip([1, 2, 3, 4, 5, 6, 3, 2, 1]).expect("valid base"),
        testdata::regon9([1, 2, 3, 4, 5, 6, 7, 8]),
        testdata::iban("PL", "109010140000071219812874"),
        testdata::card("4", 16),
        "kontakt@example.com".to_string(),
    ];

    let mut out = String::with_capacity(target_bytes + 256);
    let mut index = 0usize;
    while out.len() < target_bytes {
        out.push_str("Wniosek został przyjęty do rozpatrzenia przez zespół obsługi klienta. ");
        out.push_str(&identifiers[index % identifiers.len()]);
        out.push_str(" — dokumentacja w załączniku. ");
        index += 1;
    }
    out
}

/// Digit-heavy text: every token is a candidate, none of them validates.
fn digit_noise(target_bytes: usize) -> String {
    let mut out = String::with_capacity(target_bytes + 32);
    let mut seed = 1u64;
    while out.len() < target_bytes {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        out.push_str(&format!("{} ", seed % 100_000_000_000_000_000));
    }
    out
}

fn bench_detect(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("l1_detect");

    for size in [1024usize, 100 * 1024] {
        for (label, corpus) in [
            ("clean", clean_text(size)),
            ("with_identifiers", text_with_identifiers(size)),
            ("digit_noise", digit_noise(size)),
        ] {
            group.throughput(Throughput::Bytes(corpus.len() as u64));
            group.bench_with_input(
                BenchmarkId::new(label, format!("{}KB", size / 1024)),
                &corpus,
                |bencher, text| bencher.iter(|| detect(std::hint::black_box(text))),
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_detect);
criterion_main!(benches);
