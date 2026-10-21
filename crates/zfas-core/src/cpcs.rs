// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! CPCS (critical-path cluster scaling): **experimental**.
//!
//! Idea (after the public CPCS write-up; this is an independent implementation): a frame's
//! latency is set by a chain of threads waking each other, not by total CPU load. If the run
//! time on that chain is attributed to CPU clusters, a boost can be concentrated on the
//! clusters the chain actually ran on instead of raising every cluster equally.
//!
//! Input is scheduler events (`sched_switch`, `sched_wakeup`) from tracefs, in the same clock
//! domain as the frame timestamps. Per frame the analyzer walks back from the last thread of
//! the target process to run before the frame boundary, following who woke whom, and sums
//! run time per domain. Wait time shapes the chain but is not attributed (frequency does not
//! shorten it).
//!
//! Output is an *estimate* with a confidence value from path stability (Jaccard similarity of
//! consecutive chains) and coverage (chain run time / frame time). Below the configured
//! confidence, or with too few frames, it falls back to uniform weights, which is exactly the
//! behaviour without CPCS.

use std::collections::{BTreeSet, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::model::{Clock, Provenance, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SchedKind {
    /// `prev_tid` stops running on `cpu`, `next_tid` starts.
    Switch { prev_tid: u32, next_tid: u32 },
    /// `waker_tid` (the task running when the event fired) woke `wakee_tid`.
    Wakeup { waker_tid: u32, wakee_tid: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedEvent {
    pub ts: Timestamp,
    pub cpu: u32,
    pub kind: SchedKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CpcsConfig {
    pub enabled: bool,
    /// Below this confidence the weights fall back to uniform.
    pub min_confidence: f64,
    /// Frames needed before any non-uniform weight is reported.
    pub min_frames: usize,
    /// Chains covering less than this share of the frame time count as weak evidence.
    pub min_coverage: f64,
    /// EMA factor for per-domain shares.
    pub alpha: f64,
    /// Lowest relative weight any domain gets while CPCS is active (hedge against
    /// misattribution).
    pub floor: f64,
}

impl Default for CpcsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_confidence: 0.6,
            min_frames: 16,
            min_coverage: 0.25,
            alpha: 0.2,
            floor: 0.2,
        }
    }
}

impl CpcsConfig {
    pub fn validate(&self) -> Result<(), String> {
        for (v, n) in [
            (self.min_confidence, "min_confidence"),
            (self.min_coverage, "min_coverage"),
            (self.floor, "floor"),
        ] {
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("cpcs.{n} must be within 0..=1"));
            }
        }
        if !(self.alpha > 0.0 && self.alpha <= 1.0) {
            return Err("cpcs.alpha must be within (0, 1]".into());
        }
        if self.min_frames == 0 {
            return Err("cpcs.min_frames must be at least 1".into());
        }
        Ok(())
    }
}

/// The critical chain of one frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FramePath {
    pub tids: Vec<u32>,
    pub run_ns_per_domain: Vec<u64>,
    pub frame_ns: u64,
}

impl FramePath {
    pub fn run_ns(&self) -> u64 {
        self.run_ns_per_domain.iter().sum()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpcsOutput {
    /// Relative weight per domain, max = 1.0. Uniform (all 1.0) on fallback.
    pub weights: Vec<f64>,
    pub confidence: f64,
    pub frames: usize,
    pub fallback: bool,
    pub provenance: Provenance,
    pub reason: String,
}

#[derive(Debug, Clone, Copy)]
struct Segment {
    tid: u32,
    domain: usize,
    start: u64,
    end: u64,
}

#[derive(Debug, Clone, Copy)]
struct Wake {
    ts: u64,
    waker: u32,
    wakee: u32,
}

#[derive(Debug, Clone)]
pub struct CpcsAnalyzer {
    cfg: CpcsConfig,
    clock: Clock,
    cpu_domain: Vec<Option<usize>>,
    domains: usize,
    targets: HashSet<u32>,
    running: Vec<Option<(u32, u64)>>,
    segments: VecDeque<Segment>,
    wakes: VecDeque<Wake>,
    horizon_ns: u64,
    paths: VecDeque<FramePath>,
    shares: Option<Vec<f64>>,
    pub dropped_events: u64,
}

const MAX_HOPS: usize = 64;
const MAX_PATHS: usize = 32;

impl CpcsAnalyzer {
    /// `cpu_domain[cpu]` maps a CPU to its control-domain index.
    pub fn new(
        cfg: CpcsConfig,
        clock: Clock,
        cpu_domain: Vec<Option<usize>>,
        domains: usize,
    ) -> Self {
        let ncpu = cpu_domain.len();
        Self {
            cfg,
            clock,
            cpu_domain,
            domains,
            targets: HashSet::new(),
            running: vec![None; ncpu],
            segments: VecDeque::new(),
            wakes: VecDeque::new(),
            horizon_ns: 2_000_000_000,
            paths: VecDeque::new(),
            shares: None,
            dropped_events: 0,
        }
    }

    /// Threads of the foreground process. Changing the set resets learned weights.
    pub fn set_targets(&mut self, tids: impl IntoIterator<Item = u32>) {
        let new: HashSet<u32> = tids.into_iter().collect();
        if new.is_disjoint(&self.targets) {
            self.reset();
        }
        self.targets = new;
    }

    pub fn reset(&mut self) {
        self.running.iter_mut().for_each(|r| *r = None);
        self.segments.clear();
        self.wakes.clear();
        self.paths.clear();
        self.shares = None;
    }

    pub fn ingest(&mut self, events: &[SchedEvent]) {
        let mut latest = 0;
        for e in events {
            if e.ts.clock != self.clock {
                self.dropped_events += 1;
                continue;
            }
            let ts = e.ts.ns;
            latest = latest.max(ts);
            match e.kind {
                SchedKind::Switch { prev_tid, next_tid } => {
                    let cpu = e.cpu as usize;
                    let Some(slot) = self.running.get_mut(cpu) else {
                        self.dropped_events += 1;
                        continue;
                    };
                    if let Some((tid, start)) = slot.take() {
                        // Trust the switch's prev over our bookkeeping only if they agree.
                        if tid == prev_tid && ts > start {
                            if let Some(domain) = self.cpu_domain.get(cpu).copied().flatten() {
                                self.segments.push_back(Segment {
                                    tid,
                                    domain,
                                    start,
                                    end: ts,
                                });
                            }
                        }
                    }
                    if self.targets.contains(&next_tid) {
                        *slot = Some((next_tid, ts));
                    }
                }
                SchedKind::Wakeup {
                    waker_tid,
                    wakee_tid,
                } => {
                    if self.targets.contains(&wakee_tid) {
                        self.wakes.push_back(Wake {
                            ts,
                            waker: waker_tid,
                            wakee: wakee_tid,
                        });
                    }
                }
            }
        }
        let cutoff = latest.saturating_sub(self.horizon_ns);
        while self.segments.front().is_some_and(|s| s.end < cutoff) {
            self.segments.pop_front();
        }
        while self.wakes.front().is_some_and(|w| w.ts < cutoff) {
            self.wakes.pop_front();
        }
    }

    fn run_in(&self, tid: u32, from: u64, to: u64, acc: &mut [u64]) {
        for s in self.segments.iter().filter(|s| s.tid == tid) {
            let a = s.start.max(from);
            let b = s.end.min(to);
            if b > a {
                acc[s.domain] += b - a;
            }
        }
    }

    /// Critical chain of the frame that ended at `end` and started at `start`.
    pub fn analyze_frame(&self, start: u64, end: u64) -> Option<FramePath> {
        if end <= start {
            return None;
        }
        let tail = self
            .segments
            .iter()
            .filter(|s| self.targets.contains(&s.tid) && s.end > start && s.start < end)
            .max_by_key(|s| s.end.min(end))?
            .tid;
        let mut run = vec![0u64; self.domains];
        let mut tids = Vec::new();
        let mut node = tail;
        let mut cursor = end;
        for _ in 0..MAX_HOPS {
            let wake = self
                .wakes
                .iter()
                .rev()
                .find(|w| w.wakee == node && w.ts <= cursor && w.ts >= start)
                .copied();
            let from = wake.map(|w| w.ts).unwrap_or(start);
            self.run_in(node, from, cursor, &mut run);
            tids.push(node);
            match wake {
                Some(w) if self.targets.contains(&w.waker) && w.ts < cursor => {
                    node = w.waker;
                    cursor = w.ts;
                }
                // Woken from outside the process (binder, SurfaceFlinger, IRQ) or no wakeup in
                // this frame: the chain ends here.
                _ => break,
            }
        }
        Some(FramePath {
            tids,
            run_ns_per_domain: run,
            frame_ns: end - start,
        })
    }

    /// Analyze consecutive frame boundaries (present or ready timestamps, ascending).
    pub fn observe_frames(&mut self, boundaries: &[Timestamp]) {
        for w in boundaries.windows(2) {
            if w[0].clock != self.clock || w[1].clock != self.clock {
                continue;
            }
            let Some(path) = self.analyze_frame(w[0].ns, w[1].ns) else {
                continue;
            };
            let total = path.run_ns();
            if total > 0 {
                let frame_shares: Vec<f64> = path
                    .run_ns_per_domain
                    .iter()
                    .map(|r| *r as f64 / total as f64)
                    .collect();
                let a = self.cfg.alpha;
                self.shares = Some(match self.shares.take() {
                    None => frame_shares,
                    Some(prev) => prev
                        .iter()
                        .zip(&frame_shares)
                        .map(|(p, f)| p + a * (f - p))
                        .collect(),
                });
            }
            if self.paths.len() == MAX_PATHS {
                self.paths.pop_front();
            }
            self.paths.push_back(path);
        }
    }

    fn confidence(&self) -> f64 {
        let n = self.paths.len();
        if n < 2 {
            return 0.0;
        }
        let mut stability = 0.0;
        for w in self.paths.iter().collect::<Vec<_>>().windows(2) {
            let a: BTreeSet<u32> = w[0].tids.iter().copied().collect();
            let b: BTreeSet<u32> = w[1].tids.iter().copied().collect();
            let union = a.union(&b).count();
            stability += if union == 0 {
                0.0
            } else {
                a.intersection(&b).count() as f64 / union as f64
            };
        }
        stability /= (n - 1) as f64;
        let coverage = self
            .paths
            .iter()
            .map(|p| (p.run_ns() as f64 / p.frame_ns.max(1) as f64).min(1.0))
            .sum::<f64>()
            / n as f64;
        let cov_factor = if self.cfg.min_coverage <= 0.0 {
            1.0
        } else {
            (coverage / self.cfg.min_coverage).min(1.0)
        };
        (stability * cov_factor).clamp(0.0, 1.0)
    }

    pub fn output(&self) -> CpcsOutput {
        let uniform = vec![1.0; self.domains];
        let frames = self.paths.len();
        let confidence = self.confidence();
        let fallback = |reason: String| CpcsOutput {
            weights: uniform.clone(),
            confidence,
            frames,
            fallback: true,
            provenance: Provenance::Unavailable,
            reason,
        };
        if !self.cfg.enabled {
            return fallback("CPCS disabled".into());
        }
        if frames < self.cfg.min_frames {
            return fallback(format!(
                "{frames} of {} frames analyzed",
                self.cfg.min_frames
            ));
        }
        if confidence < self.cfg.min_confidence {
            return fallback(format!(
                "confidence {confidence:.2} below {:.2}",
                self.cfg.min_confidence
            ));
        }
        let Some(shares) = self.shares.as_ref() else {
            return fallback("no critical-path run time observed".into());
        };
        let max = shares.iter().copied().fold(0.0, f64::max);
        if max <= 0.0 {
            return fallback("no critical-path run time observed".into());
        }
        CpcsOutput {
            weights: shares
                .iter()
                .map(|s| (s / max).max(self.cfg.floor).min(1.0))
                .collect(),
            confidence,
            frames,
            fallback: false,
            provenance: Provenance::Estimated,
            reason: "critical path stable".into(),
        }
    }
}

/// Spread one boost level over domains by weight. The heaviest domain gets the full level; no
/// domain gets more than `level`.
pub fn scale_levels(level: u8, weights: &[f64]) -> Vec<u8> {
    weights
        .iter()
        .map(|w| {
            ((level as f64) * w.clamp(0.0, 1.0))
                .ceil()
                .min(level as f64) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn ts(ns: u64) -> Timestamp {
        Timestamp::new(Clock::Monotonic, ns)
    }

    fn sw(t: u64, cpu: u32, prev: u32, next: u32) -> SchedEvent {
        SchedEvent {
            ts: ts(t),
            cpu,
            kind: SchedKind::Switch {
                prev_tid: prev,
                next_tid: next,
            },
        }
    }

    fn wk(t: u64, cpu: u32, waker: u32, wakee: u32) -> SchedEvent {
        SchedEvent {
            ts: ts(t),
            cpu,
            kind: SchedKind::Wakeup {
                waker_tid: waker,
                wakee_tid: wakee,
            },
        }
    }

    /// CPUs 0-3 little (domain 0), 4-7 big (domain 1). Per 16 ms frame: the game thread (100)
    /// runs 6 ms on a big core, wakes the render thread (101) which runs 4 ms on a big core.
    /// A noisy worker (102) runs 8 ms on a little core but is not on the chain.
    fn synthetic(frames: u64) -> (CpcsAnalyzer, Vec<Timestamp>) {
        let cfg = CpcsConfig {
            enabled: true,
            ..CpcsConfig::default()
        };
        let map = (0..8).map(|c| Some(if c < 4 { 0 } else { 1 })).collect();
        let mut a = CpcsAnalyzer::new(cfg, Clock::Monotonic, map, 2);
        a.set_targets([100, 101, 102]);
        let mut ev = vec![];
        let mut bounds = vec![];
        for f in 0..frames {
            let b = f * 16 * MS;
            bounds.push(ts(b));
            ev.push(wk(b, 0, 0, 100));
            ev.push(sw(b + MS / 10, 4, 0, 100));
            ev.push(sw(b + 6 * MS, 4, 100, 0));
            ev.push(wk(b + 6 * MS, 4, 100, 101));
            ev.push(sw(b + 6 * MS + MS / 10, 5, 0, 101));
            ev.push(sw(b + 10 * MS, 5, 101, 0));
            ev.push(sw(b + MS, 1, 0, 102));
            ev.push(sw(b + 9 * MS, 1, 102, 0));
        }
        bounds.push(ts(frames * 16 * MS));
        a.ingest(&ev);
        (a, bounds)
    }

    #[test]
    fn chain_is_attributed_to_the_big_cluster() {
        let (mut a, b) = synthetic(40);
        let p = a.analyze_frame(b[5].ns, b[6].ns).unwrap();
        assert_eq!(p.tids, vec![101, 100]);
        assert_eq!(
            p.run_ns_per_domain[0], 0,
            "worker off the chain must not count"
        );
        assert!(p.run_ns_per_domain[1] > 9 * MS);
        a.observe_frames(&b);
        let out = a.output();
        assert!(!out.fallback, "{out:?}");
        assert_eq!(out.provenance, Provenance::Estimated);
        assert_eq!(out.weights[1], 1.0);
        assert!(
            (out.weights[0] - 0.2).abs() < 1e-9,
            "little cluster at floor: {out:?}"
        );
        assert_eq!(scale_levels(6, &out.weights), vec![2, 6]);
    }

    #[test]
    fn too_few_frames_or_disabled_falls_back_to_uniform() {
        let (mut a, b) = synthetic(40);
        a.observe_frames(&b[..5]);
        let out = a.output();
        assert!(out.fallback);
        assert_eq!(out.weights, vec![1.0, 1.0]);

        let (mut a, b) = synthetic(40);
        a.cfg.enabled = false;
        a.observe_frames(&b);
        assert!(a.output().fallback);
    }

    #[test]
    fn unstable_chains_lower_confidence() {
        let cfg = CpcsConfig {
            enabled: true,
            ..CpcsConfig::default()
        };
        let map = (0..8).map(|c| Some(if c < 4 { 0 } else { 1 })).collect();
        let mut a = CpcsAnalyzer::new(cfg, Clock::Monotonic, map, 2);
        a.set_targets(200..240);
        let mut ev = vec![];
        let mut bounds = vec![];
        for f in 0..40u64 {
            let b = f * 16 * MS;
            bounds.push(ts(b));
            // A different, unrelated thread is last to run in every frame.
            let tid = 200 + (f % 40) as u32;
            ev.push(sw(b + MS, (f % 8) as u32, 0, tid));
            ev.push(sw(b + 3 * MS, (f % 8) as u32, tid, 0));
        }
        bounds.push(ts(40 * 16 * MS));
        a.ingest(&ev);
        a.observe_frames(&bounds);
        let out = a.output();
        assert!(out.fallback, "{out:?}");
        assert!(out.confidence < 0.1);
    }

    #[test]
    fn foreign_clock_events_are_dropped() {
        let (mut a, _) = synthetic(1);
        let e = SchedEvent {
            ts: Timestamp::new(Clock::Boottime, 5),
            cpu: 0,
            kind: SchedKind::Switch {
                prev_tid: 0,
                next_tid: 100,
            },
        };
        a.ingest(&[e]);
        assert_eq!(a.dropped_events, 1);
    }

    #[test]
    fn scale_never_exceeds_level() {
        assert_eq!(scale_levels(5, &[1.0, 0.5, 0.0, 2.0]), vec![5, 3, 0, 5]);
        assert_eq!(scale_levels(0, &[1.0]), vec![0]);
    }
}
