//! Deterministic frame analyzer.
//!
//! Turns a stream of [`FrameEvent`]s (possibly duplicated, out of order, or with gaps) into
//! frame intervals and rolling statistics against a target frame period. It never acts on a
//! single slow frame: callers read [`WindowStats`], which carries the sample count and a
//! confidence value.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::model::{FrameEvent, Timestamp};

/// Analyzer tuning. All durations in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnalyzerConfig {
    /// Frames kept for percentile statistics.
    pub window_frames: usize,
    /// Minimum frames in the window before statistics are trusted.
    pub min_samples: usize,
    /// A frame misses its deadline when its interval exceeds `target * (1 + tolerance)`.
    pub deadline_tolerance: f64,
    /// EWMA smoothing factor for frame intervals (0 < alpha <= 1).
    pub ewma_alpha: f64,
    /// No new frame for this long means rendering paused (menu, background, screen off).
    pub idle_timeout_ns: u64,
    /// An interval longer than this is a gap (pause, loading, event loss), not a frame time.
    pub max_interval_ns: u64,
    /// Frames observed after (re)start before the analyzer reports `Active`.
    pub warmup_frames: usize,
}

impl Default for AnalyzerConfig {
    fn default() -> Self {
        Self {
            window_frames: 240,
            min_samples: 30,
            deadline_tolerance: 0.10,
            ewma_alpha: 0.10,
            idle_timeout_ns: 1_500_000_000,
            max_interval_ns: 500_000_000,
            warmup_frames: 60,
        }
    }
}

/// Rendering activity as seen by the analyzer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    /// No frames yet, or not enough after a (re)start.
    Warmup,
    /// Frames arriving regularly.
    Active,
    /// No frames for `idle_timeout`: paused, loading, backgrounded or screen off.
    Idle,
}

/// Statistics over the current window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowStats {
    pub samples: usize,
    pub target_period_ns: u64,
    pub ewma_interval_ns: Option<f64>,
    pub p50_ns: Option<u64>,
    pub p95_ns: Option<u64>,
    pub p99_ns: Option<u64>,
    /// Fraction of frames in the window that missed the deadline.
    pub miss_ratio: f64,
    /// Current run of consecutive missed deadlines.
    pub consecutive_misses: u32,
    /// Achieved frames per second over the window (from the mean interval).
    pub fps: Option<f64>,
    /// 0..=1: sample count relative to `min_samples`, reduced by detected event loss.
    pub confidence: f64,
    pub activity: Activity,
}

/// Counters for data-quality diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityCounters {
    pub accepted: u64,
    pub duplicates: u64,
    pub out_of_order: u64,
    pub clock_mismatch: u64,
    pub gaps: u64,
    /// Recorded intervals over the deadline at the time they arrived.
    pub missed: u64,
}

#[derive(Debug, Clone)]
pub struct FrameAnalyzer {
    cfg: AnalyzerConfig,
    target_period_ns: u64,
    last_present: Option<Timestamp>,
    intervals: VecDeque<(u64, bool)>,
    ewma: Option<f64>,
    consecutive_misses: u32,
    since_restart: usize,
    last_event_at: Option<Timestamp>,
    pub quality: QualityCounters,
}

impl FrameAnalyzer {
    pub fn new(cfg: AnalyzerConfig, target_fps: f64) -> Self {
        let mut a = Self {
            cfg,
            target_period_ns: 0,
            last_present: None,
            intervals: VecDeque::with_capacity(cfg.window_frames),
            ewma: None,
            consecutive_misses: 0,
            since_restart: 0,
            last_event_at: None,
            quality: QualityCounters::default(),
        };
        a.set_target_fps(target_fps);
        a
    }

    /// Change the target at runtime. Existing intervals are re-judged against the new
    /// deadline; the window itself is kept.
    pub fn set_target_fps(&mut self, fps: f64) {
        let fps = if fps.is_finite() && fps >= 1.0 {
            fps
        } else {
            60.0
        };
        self.target_period_ns = (1e9 / fps).round() as u64;
        let deadline = self.deadline_ns();
        for entry in self.intervals.iter_mut() {
            entry.1 = entry.0 > deadline;
        }
        self.consecutive_misses = self
            .intervals
            .iter()
            .rev()
            .take_while(|(_, missed)| *missed)
            .count() as u32;
    }

    pub fn target_period_ns(&self) -> u64 {
        self.target_period_ns
    }

    fn deadline_ns(&self) -> u64 {
        (self.target_period_ns as f64 * (1.0 + self.cfg.deadline_tolerance)) as u64
    }

    /// Forget timing history (foreground app changed, telemetry restarted).
    pub fn reset(&mut self) {
        self.last_present = None;
        self.intervals.clear();
        self.ewma = None;
        self.consecutive_misses = 0;
        self.since_restart = 0;
        self.last_event_at = None;
    }

    /// Ingest events in any order. Events at or before the last accepted present time are
    /// duplicates or late arrivals and are counted, not used. Returns accepted frames.
    pub fn ingest(&mut self, events: &[FrameEvent]) -> usize {
        let mut sorted: Vec<&FrameEvent> = events.iter().collect();
        sorted.sort_by_key(|e| e.present.ns);
        let mut accepted = 0;
        for e in sorted {
            if let Some(last) = self.last_present {
                if e.present.clock != last.clock {
                    self.quality.clock_mismatch += 1;
                    continue;
                }
                if e.present.ns == last.ns {
                    self.quality.duplicates += 1;
                    continue;
                }
                if e.present.ns < last.ns {
                    self.quality.out_of_order += 1;
                    continue;
                }
                let interval = e.present.ns - last.ns;
                if interval > self.cfg.max_interval_ns {
                    // A pause or lost events: restart timing without recording a frame time.
                    self.quality.gaps += 1;
                    self.since_restart = 0;
                    self.consecutive_misses = 0;
                } else {
                    self.record(interval);
                }
            }
            self.last_present = Some(e.present);
            self.last_event_at = Some(e.present);
            self.since_restart += 1;
            self.quality.accepted += 1;
            accepted += 1;
        }
        accepted
    }

    fn record(&mut self, interval: u64) {
        let missed = interval > self.deadline_ns();
        self.quality.missed += missed as u64;
        if self.intervals.len() == self.cfg.window_frames {
            self.intervals.pop_front();
        }
        self.intervals.push_back((interval, missed));
        self.consecutive_misses = if missed {
            self.consecutive_misses + 1
        } else {
            0
        };
        let a = self.cfg.ewma_alpha.clamp(0.001, 1.0);
        self.ewma = Some(match self.ewma {
            None => interval as f64,
            Some(prev) => prev + a * (interval as f64 - prev),
        });
    }

    /// Statistics at time `now` (same clock as the events; a different clock is treated as
    /// "no recent frames").
    pub fn stats(&self, now: Timestamp) -> WindowStats {
        let idle = match self.last_event_at {
            None => false,
            Some(t) => now.since(t).is_none_or(|d| d > self.cfg.idle_timeout_ns),
        };
        let activity = if idle {
            Activity::Idle
        } else if self.since_restart < self.cfg.warmup_frames
            || self.intervals.len() < self.cfg.min_samples
        {
            Activity::Warmup
        } else {
            Activity::Active
        };
        let n = self.intervals.len();
        let mut sorted: Vec<u64> = self.intervals.iter().map(|(i, _)| *i).collect();
        sorted.sort_unstable();
        let pct = |p: f64| -> Option<u64> {
            if n == 0 {
                return None;
            }
            let idx = ((p * n as f64).ceil() as usize).clamp(1, n) - 1;
            Some(sorted[idx])
        };
        let enough = n >= self.cfg.min_samples;
        let misses = self.intervals.iter().filter(|(_, m)| *m).count();
        let mean = if n > 0 {
            Some(sorted.iter().sum::<u64>() as f64 / n as f64)
        } else {
            None
        };
        let seen = self.quality.accepted + self.quality.duplicates + self.quality.out_of_order;
        let loss_penalty = if seen == 0 {
            1.0
        } else {
            1.0 - (self.quality.gaps as f64 / seen as f64).min(0.5)
        };
        let confidence = ((n as f64 / self.cfg.min_samples.max(1) as f64).min(1.0)) * loss_penalty;
        WindowStats {
            samples: n,
            target_period_ns: self.target_period_ns,
            ewma_interval_ns: self.ewma,
            p50_ns: pct(0.50),
            p95_ns: if enough { pct(0.95) } else { None },
            p99_ns: if n >= 100 { pct(0.99) } else { None },
            miss_ratio: if n > 0 { misses as f64 / n as f64 } else { 0.0 },
            consecutive_misses: self.consecutive_misses,
            fps: mean.filter(|m| *m > 0.0).map(|m| 1e9 / m),
            confidence,
            activity,
        }
    }

    /// Raw intervals in the window, oldest first (for diagnostics and target estimation).
    pub fn intervals(&self) -> impl Iterator<Item = u64> + '_ {
        self.intervals.iter().map(|(i, _)| *i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clock, TelemetrySource};

    pub(crate) fn frame(ns: u64) -> FrameEvent {
        FrameEvent {
            source: TelemetrySource::Synthetic,
            present: Timestamp::new(Clock::Synthetic, ns),
            ready: None,
            desired_present: None,
        }
    }

    fn feed(a: &mut FrameAnalyzer, start: u64, period: u64, n: usize) -> u64 {
        let ev: Vec<_> = (0..n as u64).map(|i| frame(start + i * period)).collect();
        a.ingest(&ev);
        start + n as u64 * period
    }

    #[test]
    fn steady_60fps_meets_deadline() {
        let mut a = FrameAnalyzer::new(AnalyzerConfig::default(), 60.0);
        let end = feed(&mut a, 0, 16_666_667, 200);
        let s = a.stats(Timestamp::new(Clock::Synthetic, end));
        assert_eq!(s.activity, Activity::Active);
        assert_eq!(s.miss_ratio, 0.0);
        assert!((s.fps.unwrap() - 60.0).abs() < 0.1);
        assert_eq!(s.confidence, 1.0);
    }

    #[test]
    fn duplicates_and_out_of_order_are_counted_not_used() {
        let mut a = FrameAnalyzer::new(AnalyzerConfig::default(), 60.0);
        a.ingest(&[frame(100), frame(16_666_767)]);
        a.ingest(&[frame(16_666_767), frame(50)]);
        assert_eq!(a.quality.duplicates, 1);
        assert_eq!(a.quality.out_of_order, 1);
        assert_eq!(a.intervals().count(), 1);
    }

    #[test]
    fn gap_restarts_timing_instead_of_recording_a_huge_frame() {
        let mut a = FrameAnalyzer::new(AnalyzerConfig::default(), 60.0);
        let end = feed(&mut a, 0, 16_666_667, 100);
        feed(&mut a, end + 2_000_000_000, 16_666_667, 10);
        assert_eq!(a.quality.gaps, 1);
        assert!(a.intervals().all(|i| i < 20_000_000));
        let s = a.stats(Timestamp::new(Clock::Synthetic, end + 2_200_000_000));
        assert_eq!(s.activity, Activity::Warmup, "warm-up restarts after a gap");
    }

    #[test]
    fn idle_after_timeout() {
        let mut a = FrameAnalyzer::new(AnalyzerConfig::default(), 60.0);
        let end = feed(&mut a, 0, 16_666_667, 100);
        let s = a.stats(Timestamp::new(Clock::Synthetic, end + 2_000_000_000));
        assert_eq!(s.activity, Activity::Idle);
    }

    #[test]
    fn target_change_rejudges_window() {
        let mut a = FrameAnalyzer::new(AnalyzerConfig::default(), 60.0);
        let end = feed(&mut a, 0, 16_666_667, 100);
        a.set_target_fps(120.0);
        let s = a.stats(Timestamp::new(Clock::Synthetic, end));
        assert_eq!(s.miss_ratio, 1.0);
        assert_eq!(s.target_period_ns, 8_333_333);
    }

    #[test]
    fn mixed_clocks_are_rejected() {
        let mut a = FrameAnalyzer::new(AnalyzerConfig::default(), 60.0);
        a.ingest(&[frame(0)]);
        let mut other = frame(16_000_000);
        other.present.clock = Clock::Monotonic;
        a.ingest(&[other]);
        assert_eq!(a.quality.clock_mismatch, 1);
    }

    #[test]
    fn percentiles_reflect_tail() {
        let mut a = FrameAnalyzer::new(AnalyzerConfig::default(), 60.0);
        let mut t = 0;
        let mut ev = vec![frame(0)];
        for i in 0..200u64 {
            t += if i % 20 == 0 { 40_000_000 } else { 16_666_667 };
            ev.push(frame(t));
        }
        a.ingest(&ev);
        let s = a.stats(Timestamp::new(Clock::Synthetic, t));
        assert_eq!(s.p50_ns, Some(16_666_667));
        assert_eq!(s.p99_ns, Some(40_000_000));
        assert!((s.miss_ratio - 0.05).abs() < 1e-9);
    }
}
