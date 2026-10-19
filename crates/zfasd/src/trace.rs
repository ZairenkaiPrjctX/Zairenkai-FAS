//! Session traces: record live observations, replay them through the engine offline, and
//! generate clearly labelled synthetic traces for tests and benchmarks.
//!
//! Format: JSON Lines. Line 1 is a [`Header`]; every other line is one engine
//! [`StepInput`]. Frame events carry their `source`, so a synthetic trace can never be
//! mistaken for recorded hardware telemetry.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use zfas_core::config::Config;
use zfas_core::control::{DomainInfo, DomainKind, MemoryBackend};
use zfas_core::engine::{Engine, StepInput};
use zfas_core::model::{Clock, FrameEvent, LoadEvidence, TelemetrySource, Timestamp};
use zfas_core::thermal::{ThermalInput, ThermalStatus};

pub const FORMAT: &str = "zairenkai-fas-trace";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub format: String,
    pub version: u32,
    /// `recorded` (live device) or `synthetic`.
    pub origin: String,
    pub package: Option<String>,
    pub domains: Vec<DomainInfo>,
    pub tick_ms: u64,
    pub note: String,
}

pub fn write_trace(path: &Path, header: &Header, steps: &[StepInput]) -> std::io::Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    serde_json::to_writer(&mut f, header)?;
    f.write_all(b"\n")?;
    for s in steps {
        serde_json::to_writer(&mut f, s)?;
        f.write_all(b"\n")?;
    }
    f.flush()
}

pub fn read_trace(path: &Path) -> Result<(Header, Vec<StepInput>), String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = BufReader::new(f).lines();
    let first = lines
        .next()
        .ok_or("empty trace")?
        .map_err(|e| e.to_string())?;
    let header: Header = serde_json::from_str(&first).map_err(|e| format!("header: {e}"))?;
    if header.format != FORMAT || header.version != 1 {
        return Err(format!(
            "unsupported trace format {} v{}",
            header.format, header.version
        ));
    }
    let mut steps = Vec::new();
    for (i, l) in lines.enumerate() {
        let l = l.map_err(|e| e.to_string())?;
        if l.trim().is_empty() {
            continue;
        }
        steps.push(serde_json::from_str(&l).map_err(|e| format!("line {}: {e}", i + 2))?);
    }
    Ok((header, steps))
}

#[derive(Debug, Serialize)]
pub struct ReplaySummary {
    pub origin: String,
    pub note: &'static str,
    pub ticks: usize,
    pub diag: zfas_core::diag::SessionDiag,
    pub max_level_per_domain: Vec<u8>,
    pub final_state: String,
    pub targets_seen: Vec<u32>,
}

/// Replay through the real engine with an in-memory backend. Returns per-tick outputs as
/// JSON values when `timeline` is set.
pub fn replay(
    cfg: Config,
    header: &Header,
    steps: Vec<StepInput>,
    timeline: bool,
) -> (ReplaySummary, Vec<serde_json::Value>) {
    let app = cfg.for_app(header.package.as_deref());
    let mut backend = MemoryBackend::new(header.domains.clone());
    let mut engine = Engine::new(cfg, app, header.domains.clone(), Clock::Monotonic);
    let mut max = vec![0u8; header.domains.len()];
    let mut out = Vec::new();
    let mut targets = Vec::new();
    let mut last_state = String::new();
    let n = steps.len();
    for s in steps {
        let o = engine.step(s, &mut backend);
        for (m, l) in max.iter_mut().zip(&o.levels) {
            *m = (*m).max(*l);
        }
        if let Some(t) = o.target_fps {
            if !targets.contains(&t) {
                targets.push(t);
            }
        }
        last_state = format!("{:?}", o.decision.state);
        if timeline {
            out.push(serde_json::to_value(&o).expect("serializable"));
        }
    }
    let _ = engine.shutdown(&mut backend);
    (
        ReplaySummary {
            origin: header.origin.clone(),
            note: "replay: levels are what the controller would have requested; no hardware was touched",
            ticks: n,
            diag: engine.diag.clone(),
            max_level_per_domain: max,
            final_state: last_state,
            targets_seen: targets,
        },
        out,
    )
}

pub fn synthetic_domains() -> Vec<DomainInfo> {
    vec![
        DomainInfo {
            id: "synthetic/little".into(),
            kind: DomainKind::Cpu,
            cpus: vec![0, 1, 2, 3],
            capacity_khz: 1_800_000,
        },
        DomainInfo {
            id: "synthetic/big".into(),
            kind: DomainKind::Cpu,
            cpus: vec![4, 5, 6],
            capacity_khz: 2_400_000,
        },
        DomainInfo {
            id: "synthetic/prime".into(),
            kind: DomainKind::Cpu,
            cpus: vec![7],
            capacity_khz: 3_000_000,
        },
    ]
}

pub const SCENARIOS: [&str; 4] = ["steady60", "cpu_drop", "thermal", "pause"];

/// Synthetic scenario on a 120 Hz panel, 500 ms ticks. Every frame is tagged `synthetic`.
pub fn synthesize(scenario: &str) -> Result<(Header, Vec<StepInput>), String> {
    let period_120 = 8_333_333u64;
    let mut steps = Vec::new();
    let mut t = 1_000_000_000u64;
    let mut next_frame = t;
    // (seconds, fps (0 = paused), cpu busy, thermal status)
    let phases: Vec<(u64, f64, f64, ThermalStatus)> = match scenario {
        "steady60" => vec![(60, 60.0, 0.55, ThermalStatus::None)],
        "cpu_drop" => vec![
            (15, 60.0, 0.6, ThermalStatus::None),
            (25, 48.0, 0.95, ThermalStatus::None),
            (20, 60.0, 0.6, ThermalStatus::None),
        ],
        "thermal" => vec![
            (15, 48.0, 0.95, ThermalStatus::None),
            (10, 48.0, 0.95, ThermalStatus::Moderate),
            (10, 48.0, 0.95, ThermalStatus::Severe),
            (25, 48.0, 0.95, ThermalStatus::None),
        ],
        "pause" => vec![
            (20, 48.0, 0.95, ThermalStatus::None),
            (10, 0.0, 0.05, ThermalStatus::None),
            (20, 48.0, 0.95, ThermalStatus::None),
        ],
        other => {
            return Err(format!(
                "unknown scenario {other:?}; known: {}",
                SCENARIOS.join(", ")
            ))
        }
    };
    for (secs, fps, busy, status) in phases {
        let end = t + secs * 1_000_000_000;
        while t < end {
            t += 500_000_000;
            let mut frames = Vec::new();
            if fps > 0.0 {
                let period = (1e9 / fps) as u64;
                if next_frame + period < t.saturating_sub(500_000_000) {
                    next_frame = t - 500_000_000;
                }
                while next_frame <= t {
                    frames.push(FrameEvent {
                        source: TelemetrySource::Synthetic,
                        present: Timestamp::new(Clock::Monotonic, next_frame),
                        ready: None,
                        desired_present: None,
                    });
                    next_frame += period;
                }
            }
            steps.push(StepInput {
                now: Timestamp::new(Clock::Monotonic, t),
                frames,
                refresh_period_ns: Some(period_120),
                telemetry_ok: true,
                evidence: LoadEvidence {
                    max_thread_busy: Some(busy),
                    gpu_busy: Some(0.4),
                    cpu_max_capped: Some(false),
                },
                composition_late: Some(false),
                thermal: ThermalInput {
                    status: Some(status),
                    zones: vec![],
                    read_error: false,
                },
                sched: vec![],
            });
        }
    }
    let header = Header {
        format: FORMAT.into(),
        version: 1,
        origin: "synthetic".into(),
        package: Some("synthetic.scenario".into()),
        domains: synthetic_domains(),
        tick_ms: 500,
        note: format!("synthetic scenario '{scenario}': generated, not measured"),
    };
    Ok((header, steps))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_60() -> Config {
        let mut c = Config::default();
        c.apps.insert(
            "synthetic.scenario".into(),
            zfas_core::config::AppOverride {
                target_fps: vec![60],
                ..Default::default()
            },
        );
        c
    }

    #[test]
    fn trace_roundtrip() {
        let d = zfas_platform::testutil::TempDir::new("trace");
        let (h, s) = synthesize("steady60").unwrap();
        let p = d.path().join("t.jsonl");
        write_trace(&p, &h, &s).unwrap();
        let (h2, s2) = read_trace(&p).unwrap();
        assert_eq!(h, h2);
        assert_eq!(s, s2);
    }

    #[test]
    fn scenarios_behave() {
        let run = |name: &str| {
            let (h, s) = synthesize(name).unwrap();
            replay(cfg_60(), &h, s, false).0
        };
        let steady = run("steady60");
        assert_eq!(steady.max_level_per_domain, vec![0, 0, 0]);
        assert_eq!(steady.diag.boost_steps, 0);

        let drop = run("cpu_drop");
        assert!(
            drop.max_level_per_domain.iter().all(|l| *l > 0 && *l <= 6),
            "{:?}",
            drop.max_level_per_domain
        );
        assert_eq!(drop.final_state, "Normal", "boost released after recovery");

        let thermal = run("thermal");
        assert!(
            thermal
                .diag
                .state_ms
                .get("thermal_limited")
                .copied()
                .unwrap_or(0)
                >= 5_000
        );

        let pause = run("pause");
        assert!(pause.diag.state_ms.get("paused").copied().unwrap_or(0) >= 5_000);
    }
}
