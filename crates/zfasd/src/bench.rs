// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! Engine overhead benchmark on synthetic input. Measures this host's CPU time per engine
//! step; it says nothing about FPS or power on a device.

use std::time::Instant;

use serde::Serialize;
use zfas_core::config::Config;
use zfas_core::control::MemoryBackend;
use zfas_core::engine::Engine;
use zfas_core::model::Clock;

use crate::trace::synthesize;

#[derive(Debug, Serialize)]
pub struct BenchResult {
    pub note: &'static str,
    pub scenario: String,
    pub steps: usize,
    pub mean_us: f64,
    pub p99_us: f64,
    pub max_us: f64,
}

pub fn bench(scenario: &str, rounds: usize) -> Result<BenchResult, String> {
    let (h, steps) = synthesize(scenario)?;
    let mut samples = Vec::with_capacity(steps.len() * rounds);
    for _ in 0..rounds.max(1) {
        let cfg = Config::default();
        let app = cfg.for_app(h.package.as_deref());
        let mut b = MemoryBackend::new(h.domains.clone());
        let mut e = Engine::new(cfg, app, h.domains.clone(), Clock::Monotonic);
        for s in steps.iter().cloned() {
            let t0 = Instant::now();
            std::hint::black_box(e.step(s, &mut b));
            samples.push(t0.elapsed().as_nanos() as f64 / 1_000.0);
        }
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples.len();
    Ok(BenchResult {
        note: "host CPU time per engine step on synthetic input; excludes dumpsys/sysfs I/O; not a device FPS or power result",
        scenario: scenario.into(),
        steps: n,
        mean_us: samples.iter().sum::<f64>() / n as f64,
        p99_us: samples[((n as f64 * 0.99) as usize).min(n - 1)],
        max_us: samples[n - 1],
    })
}
