//! Frame telemetry from `dumpsys SurfaceFlinger --latency <layer>`.
//!
//! Output format (AOSP `SurfaceFlinger::dumpStatsLocked` / `FrameTracker`): the first line is
//! the display refresh period in ns, then one line per recent frame with three
//! `CLOCK_MONOTONIC` timestamps: desired present, actual present, frame ready. Unused slots
//! are all zero; a fence that has not signalled yet is `INT64_MAX`. The history is a ring of
//! ~128 frames, so the backend must poll faster than 128 frames elapse and de-duplicate,
//! which the analyzer does.

use std::time::Duration;

use zfas_core::model::{Clock, FrameEvent, TelemetrySource, Timestamp};

use crate::exec::{ExecError, Runner};

const PENDING: i64 = i64::MAX;

#[derive(Debug, Clone, PartialEq)]
pub struct LatencyDump {
    pub refresh_period_ns: Option<u64>,
    pub frames: Vec<FrameEvent>,
    /// Lines that did not parse (format drift indicator).
    pub malformed: usize,
    /// Frames whose present fence had not signalled yet.
    pub pending: usize,
}

fn ts(v: i64) -> Option<Timestamp> {
    (v > 0 && v != PENDING).then(|| Timestamp::new(Clock::Monotonic, v as u64))
}

pub fn parse_latency(text: &str) -> LatencyDump {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let refresh_period_ns = lines
        .next()
        .and_then(|l| l.parse::<u64>().ok())
        .filter(|p| *p > 0);
    let mut frames = Vec::new();
    let mut malformed = 0;
    let mut pending = 0;
    for l in lines {
        let nums: Vec<i64> = l
            .split_whitespace()
            .filter_map(|t| t.parse().ok())
            .collect();
        if nums.len() != 3 || l.split_whitespace().count() != 3 {
            malformed += 1;
            continue;
        }
        let (desired, actual, ready) = (nums[0], nums[1], nums[2]);
        if desired == 0 && actual == 0 && ready == 0 {
            continue;
        }
        let Some(present) = ts(actual) else {
            pending += 1;
            continue;
        };
        frames.push(FrameEvent {
            source: TelemetrySource::SurfaceFlingerLatency,
            present,
            ready: ts(ready),
            desired_present: ts(desired),
        });
    }
    LatencyDump {
        refresh_period_ns,
        frames,
        malformed,
        pending,
    }
}

/// Share of frames whose buffer was ready before its desired present time but which reached
/// the display more than one refresh period late: lateness the app could not have caused.
pub fn composition_late_ratio(frames: &[FrameEvent], refresh_period_ns: u64) -> Option<f64> {
    let judged: Vec<bool> = frames
        .iter()
        .filter_map(|f| {
            let (ready, desired) = (f.ready?, f.desired_present?);
            if ready.clock != f.present.clock || desired.clock != f.present.clock {
                return None;
            }
            if ready.ns > desired.ns {
                return None; // app was late; not a composition problem
            }
            Some(f.present.ns.saturating_sub(desired.ns) > refresh_period_ns)
        })
        .collect();
    if judged.len() < 10 {
        return None;
    }
    Some(judged.iter().filter(|l| **l).count() as f64 / judged.len() as f64)
}

/// Layers from `dumpsys SurfaceFlinger --list`.
pub fn parse_layer_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.ends_with(':') && !l.starts_with("Display "))
        .map(str::to_string)
        .collect()
}

/// Candidate layers of `package`, best first: game-style `SurfaceView` layers, then
/// BLAST/app window layers. Decor, starting windows and background layers are excluded.
pub fn candidate_layers(layers: &[String], package: &str) -> Vec<String> {
    let mut c: Vec<(u8, &String)> = layers
        .iter()
        .filter(|l| {
            l.contains(package)
                && !l.contains("Background for")
                && !l.starts_with("Splash Screen")
                && !l.contains("Starting ")
                && !l.contains("Bounds for")
        })
        .map(|l| {
            let rank = if l.starts_with("SurfaceView[") || l.starts_with("SurfaceView -") {
                if l.contains("BLAST") {
                    0
                } else {
                    1
                }
            } else if l.contains("BLAST") {
                2
            } else {
                3
            };
            (rank, l)
        })
        .collect();
    c.sort_by_key(|(r, _)| *r);
    c.into_iter().map(|(_, l)| l.clone()).collect()
}

pub struct SfLatencySource<'a> {
    runner: &'a dyn Runner,
    timeout: Duration,
    layer: Option<String>,
}

#[derive(Debug)]
pub enum TelemetryError {
    Exec(ExecError),
    NoLayer(String),
    NoFrames(String),
}

impl std::fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TelemetryError::Exec(e) => write!(f, "dumpsys SurfaceFlinger: {e}"),
            TelemetryError::NoLayer(p) => write!(f, "no SurfaceFlinger layer found for {p}"),
            TelemetryError::NoFrames(l) => write!(f, "layer {l} reports no frames"),
        }
    }
}

impl<'a> SfLatencySource<'a> {
    pub fn new(runner: &'a dyn Runner) -> Self {
        Self {
            runner,
            timeout: Duration::from_millis(1_500),
            layer: None,
        }
    }

    pub fn layer(&self) -> Option<&str> {
        self.layer.as_deref()
    }

    pub fn forget_layer(&mut self) {
        self.layer = None;
    }

    fn latency(&self, layer: &str) -> Result<LatencyDump, TelemetryError> {
        let out = self
            .runner
            .run(
                &["dumpsys", "SurfaceFlinger", "--latency", layer],
                self.timeout,
            )
            .map_err(TelemetryError::Exec)?;
        Ok(parse_latency(&out))
    }

    /// Poll the foreground app's frames, (re)selecting its layer when needed.
    pub fn poll(&mut self, package: &str) -> Result<LatencyDump, TelemetryError> {
        if let Some(l) = self.layer.clone() {
            let d = self.latency(&l)?;
            if !d.frames.is_empty() {
                return Ok(d);
            }
            self.layer = None;
        }
        let list = self
            .runner
            .run(&["dumpsys", "SurfaceFlinger", "--list"], self.timeout)
            .map_err(TelemetryError::Exec)?;
        let candidates = candidate_layers(&parse_layer_list(&list), package);
        if candidates.is_empty() {
            return Err(TelemetryError::NoLayer(package.to_string()));
        }
        for l in candidates.iter().take(4) {
            let d = self.latency(l)?;
            if !d.frames.is_empty() {
                self.layer = Some(l.clone());
                return Ok(d);
            }
        }
        Err(TelemetryError::NoFrames(candidates[0].clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::FakeRunner;

    const DUMP: &str = "16666666\n\
        0\t0\t0\n\
        1000000000\t1000100000\t995000000\n\
        1016666666\t1016766666\t1012000000\n\
        1033333333\t9223372036854775807\t1030000000\n\
        garbage line here\n";

    #[test]
    fn parses_frames_pending_and_malformed() {
        let d = parse_latency(DUMP);
        assert_eq!(d.refresh_period_ns, Some(16_666_666));
        assert_eq!(d.frames.len(), 2);
        assert_eq!(d.pending, 1);
        assert_eq!(d.malformed, 1);
        assert_eq!(
            d.frames[0].present,
            Timestamp::new(Clock::Monotonic, 1_000_100_000)
        );
        assert_eq!(d.frames[0].ready.unwrap().ns, 995_000_000);
    }

    #[test]
    fn layer_selection_prefers_surfaceview() {
        let list = "Display 0 (active) HWC layers:\n\
            com.example.game/com.example.game.MainActivity#120\n\
            SurfaceView[com.example.game/com.example.game.MainActivity](BLAST)#130\n\
            Background for SurfaceView[com.example.game/com.example.game.MainActivity]#131\n\
            StatusBar#40\n";
        let c = candidate_layers(&parse_layer_list(list), "com.example.game");
        assert_eq!(
            c[0],
            "SurfaceView[com.example.game/com.example.game.MainActivity](BLAST)#130"
        );
        assert_eq!(c.len(), 2);
        assert!(candidate_layers(&parse_layer_list(list), "com.other").is_empty());
    }

    #[test]
    fn poll_falls_back_to_layer_with_frames() {
        let sv = "SurfaceView[com.g/com.g.A](BLAST)#2";
        let app = "com.g/com.g.A#1";
        let r = FakeRunner::default()
            .with("dumpsys SurfaceFlinger --list", &format!("{app}\n{sv}\n"))
            .with(
                &format!("dumpsys SurfaceFlinger --latency {sv}"),
                "16666666\n0 0 0\n",
            )
            .with(&format!("dumpsys SurfaceFlinger --latency {app}"), DUMP);
        let mut s = SfLatencySource::new(&r);
        let d = s.poll("com.g").unwrap();
        assert_eq!(d.frames.len(), 2);
        assert_eq!(s.layer(), Some(app));
    }

    #[test]
    fn composition_lateness() {
        let mk = |desired: u64, present: u64, ready: u64| FrameEvent {
            source: TelemetrySource::Synthetic,
            present: Timestamp::new(Clock::Monotonic, present),
            ready: Some(Timestamp::new(Clock::Monotonic, ready)),
            desired_present: Some(Timestamp::new(Clock::Monotonic, desired)),
        };
        let p = 16_666_666;
        let late: Vec<_> = (1..21)
            .map(|i| mk(i * p, i * p + 2 * p, i * p - 1))
            .collect();
        assert_eq!(composition_late_ratio(&late, p), Some(1.0));
        let ok: Vec<_> = (1..21).map(|i| mk(i * p, i * p + 100, i * p - 1)).collect();
        assert_eq!(composition_late_ratio(&ok, p), Some(0.0));
        assert_eq!(composition_late_ratio(&ok[..5], p), None);
    }
}
