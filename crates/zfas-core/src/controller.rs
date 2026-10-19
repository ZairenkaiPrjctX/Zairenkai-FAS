//! Adaptive FAS controller: a deterministic state machine from frame statistics, hardware
//! evidence and thermal limits to a bounded boost level.
//!
//! Safety properties (each covered by a test below):
//! - a single slow frame never raises the level;
//! - the level never exceeds the profile maximum or the thermal cap;
//! - CPU is not boosted when the evidence says CPU is not the bottleneck;
//! - every boost relaxes within `max_boost_ms` unless re-justified, and returns to 0 after
//!   recovery, pause, telemetry loss, thermal suspension or a control fault.

use serde::{Deserialize, Serialize};

use crate::analyzer::{Activity, WindowStats};
use crate::model::LoadEvidence;
use crate::policy::ProfileParams;
use crate::thermal::ThermalLimit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerState {
    Disabled,
    Initializing,
    Warmup,
    Normal,
    /// Rendering paused, loading or backgrounded: no frames to judge.
    Paused,
    Degraded,
    BoostShort,
    BoostSustained,
    Recovery,
    ThermalLimited,
    TelemetryUnavailable,
    /// Control writes failed or were overridden by another agent; boosting stopped.
    SafeFallback,
}

/// Most likely cause of missed deadlines. Always an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bottleneck {
    CpuBound,
    GpuBound,
    /// Neither CPU nor GPU is saturated: the app waits (I/O, locks, game logic pacing).
    AppOrSyncWait,
    /// Buffers were ready in time but presented late (SurfaceFlinger/composition/display).
    Composition,
    /// CPU maximum is capped by thermal or power management.
    ThermalOrPower,
    Unknown,
}

/// Classify from whatever evidence exists. Thresholds are deliberately wide: anything not
/// clearly attributable is `Unknown`, which the controller treats conservatively.
pub fn classify(ev: &LoadEvidence, composition_late: Option<bool>) -> Bottleneck {
    if ev.cpu_max_capped == Some(true) {
        return Bottleneck::ThermalOrPower;
    }
    if composition_late == Some(true) {
        return Bottleneck::Composition;
    }
    let cpu = ev.max_thread_busy;
    let gpu = ev.gpu_busy;
    match (cpu, gpu) {
        (Some(c), _) if c >= 0.85 => Bottleneck::CpuBound,
        (Some(c), Some(g)) if g >= 0.90 && c < 0.85 => Bottleneck::GpuBound,
        (None, Some(g)) if g >= 0.90 => Bottleneck::GpuBound,
        (Some(c), Some(g)) if c < 0.50 && g < 0.60 => Bottleneck::AppOrSyncWait,
        _ => Bottleneck::Unknown,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControllerInput {
    pub now_ms: u64,
    pub enabled: bool,
    pub telemetry_ok: bool,
    /// The control backend failed to apply or verify a value, or another agent overrode it.
    pub control_fault: bool,
    pub stats: WindowStats,
    pub evidence: LoadEvidence,
    pub composition_late: Option<bool>,
    pub thermal: ThermalLimit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub state: ControllerState,
    /// CPU boost level 0..=MAX_LEVEL (0 = no boost, original settings).
    pub cpu_level: u8,
    /// GPU boost level, only used when a GPU control backend is enabled.
    pub gpu_level: u8,
    pub bottleneck: Bottleneck,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct Controller {
    params: ProfileParams,
    state: ControllerState,
    level: u8,
    gpu_level: u8,
    degraded_ticks: u32,
    last_increase_ms: Option<u64>,
    boost_started_ms: Option<u64>,
    short_until_ms: Option<u64>,
    stable_since_ms: Option<u64>,
    /// (level before the step, miss ratio before the step, tick count since) for ADAPTIVE.
    pending_check: Option<(u8, f64, u32)>,
    ineffective_until_ms: Option<u64>,
}

impl Controller {
    pub fn new(params: ProfileParams) -> Self {
        Self {
            params,
            state: ControllerState::Initializing,
            level: 0,
            gpu_level: 0,
            degraded_ticks: 0,
            last_increase_ms: None,
            boost_started_ms: None,
            short_until_ms: None,
            stable_since_ms: None,
            pending_check: None,
            ineffective_until_ms: None,
        }
    }

    pub fn set_params(&mut self, params: ProfileParams) {
        self.params = params;
        self.level = self.level.min(params.max_level);
    }

    pub fn state(&self) -> ControllerState {
        self.state
    }

    /// Drop all boosts and history (foreground app change, reset command).
    pub fn reset(&mut self) {
        let p = self.params;
        *self = Self::new(p);
    }

    fn relax_all(&mut self, state: ControllerState) {
        self.state = state;
        self.level = 0;
        self.gpu_level = 0;
        self.degraded_ticks = 0;
        self.boost_started_ms = None;
        self.short_until_ms = None;
        self.pending_check = None;
    }

    fn decision(&self, bottleneck: Bottleneck, reason: impl Into<String>) -> Decision {
        Decision {
            state: self.state,
            cpu_level: self.level,
            gpu_level: self.gpu_level,
            bottleneck,
            reason: reason.into(),
        }
    }

    pub fn tick(&mut self, input: &ControllerInput) -> Decision {
        let p = self.params;
        let now = input.now_ms;
        if !input.enabled {
            self.relax_all(ControllerState::Disabled);
            return self.decision(Bottleneck::Unknown, "FAS disabled");
        }
        if input.control_fault {
            self.relax_all(ControllerState::SafeFallback);
            return self.decision(
                Bottleneck::Unknown,
                "control backend fault: boosts released",
            );
        }
        if !input.telemetry_ok {
            self.relax_all(ControllerState::TelemetryUnavailable);
            return self.decision(
                Bottleneck::Unknown,
                "no valid frame telemetry: boosts released",
            );
        }
        if input.thermal.suspended {
            self.relax_all(ControllerState::ThermalLimited);
            return self.decision(
                Bottleneck::ThermalOrPower,
                format!("thermal: {}", input.thermal.reason),
            );
        }
        match input.stats.activity {
            Activity::Warmup => {
                self.relax_all(ControllerState::Warmup);
                return self.decision(Bottleneck::Unknown, "collecting frames before acting");
            }
            Activity::Idle => {
                self.relax_all(ControllerState::Paused);
                return self.decision(Bottleneck::Unknown, "rendering paused or loading");
            }
            Activity::Active => {}
        }

        let s = &input.stats;
        let bottleneck = classify(&input.evidence, input.composition_late);
        let cap = p.max_level.min(input.thermal.level_cap);
        let cpu_cap = match bottleneck {
            Bottleneck::CpuBound => cap,
            Bottleneck::Unknown => cap.min(p.unknown_max_level),
            // CPU frequency does not fix these; do not burn power trying.
            _ => 0,
        };
        let confident = s.confidence >= p.min_confidence;
        let degraded = confident && s.miss_ratio >= p.degrade_miss_ratio;
        let recovered = confident && s.miss_ratio <= p.recover_miss_ratio;
        let burst = s.consecutive_misses >= p.consecutive_trigger;

        // ADAPTIVE: judge the effect of the last step after `eval_ticks` more ticks.
        if let Some((prev_level, prev_ratio, ticks)) = self.pending_check {
            if ticks + 1 >= p.eval_ticks.max(2) {
                self.pending_check = None;
                if p.track_effectiveness
                    && s.miss_ratio > prev_ratio * 0.9
                    && self.level > prev_level
                {
                    self.level = prev_level;
                    self.ineffective_until_ms = Some(now + p.max_boost_ms);
                    self.state = ControllerState::Degraded;
                    return self.decision(
                        bottleneck,
                        format!(
                            "last boost step did not reduce misses ({:.0}% -> {:.0}%); reverted",
                            prev_ratio * 100.0,
                            s.miss_ratio * 100.0
                        ),
                    );
                }
            } else {
                self.pending_check = Some((prev_level, prev_ratio, ticks + 1));
            }
        }

        // Enforce caps every tick (thermal cap may have dropped, bottleneck may have moved).
        if self.level > cpu_cap {
            self.level = cpu_cap;
        }
        let thermal_clamped = input.thermal.level_cap < p.max_level
            && degraded
            && self.level >= cpu_cap
            && cpu_cap > 0;

        // Bound every boost in time.
        if let Some(start) = self.boost_started_ms {
            if self.level > 0 && now.saturating_sub(start) >= p.max_boost_ms {
                self.level = self.level.saturating_sub(p.step_down);
                self.boost_started_ms = if self.level > 0 { Some(now) } else { None };
                self.state = ControllerState::Recovery;
                return self.decision(
                    bottleneck,
                    "maximum boost duration reached: relaxing one step",
                );
            }
        }

        if degraded {
            self.stable_since_ms = None;
            self.degraded_ticks += 1;
            let blocked = self.ineffective_until_ms.is_some_and(|t| now < t);
            let cooled = self
                .last_increase_ms
                .is_none_or(|t| now.saturating_sub(t) >= p.cooldown_ms);
            if cpu_cap == 0 {
                self.level = 0;
                self.boost_started_ms = None;
                self.state = ControllerState::Degraded;
                return self.decision(bottleneck, format!("missing deadlines but bottleneck is {bottleneck:?}: CPU boost would not help"));
            }
            if self.degraded_ticks >= p.eval_ticks && cooled && !blocked && self.level < cpu_cap {
                let before = self.level;
                self.level = (self.level + p.step_up).min(cpu_cap);
                self.last_increase_ms = Some(now);
                self.boost_started_ms.get_or_insert(now);
                self.short_until_ms = None;
                if p.track_effectiveness {
                    self.pending_check = Some((before, s.miss_ratio, 0));
                }
                self.state = ControllerState::BoostSustained;
                return self.decision(
                    bottleneck,
                    format!(
                        "sustained misses {:.0}% for {} ticks: level {before} -> {}",
                        s.miss_ratio * 100.0,
                        self.degraded_ticks,
                        self.level
                    ),
                );
            }
            if thermal_clamped {
                self.state = ControllerState::ThermalLimited;
                return self.decision(
                    bottleneck,
                    format!(
                        "boost held at thermal cap {}: {}",
                        input.thermal.level_cap, input.thermal.reason
                    ),
                );
            }
            if self.level == 0 {
                self.state = ControllerState::Degraded;
            }
            let why = if blocked {
                "boosting paused: recent step was ineffective"
            } else {
                "degraded; waiting for evaluation window or cooldown"
            };
            return self.decision(bottleneck, why);
        }
        self.degraded_ticks = 0;

        if burst
            && self.level < p.short_level.min(cpu_cap)
            && self.state != ControllerState::BoostSustained
        {
            self.level = p.short_level.min(cpu_cap);
            self.short_until_ms = Some(now + p.short_boost_ms);
            self.boost_started_ms.get_or_insert(now);
            self.state = ControllerState::BoostShort;
            return self.decision(
                bottleneck,
                format!(
                    "{} consecutive missed deadlines: short boost",
                    s.consecutive_misses
                ),
            );
        }
        if let Some(until) = self.short_until_ms {
            if now < until {
                return self.decision(bottleneck, "short boost active");
            }
            self.short_until_ms = None;
            if self.state == ControllerState::BoostShort {
                self.level = 0;
                self.boost_started_ms = None;
                self.state = ControllerState::Normal;
                return self.decision(bottleneck, "short boost expired");
            }
        }

        if self.level > 0 {
            if recovered {
                let since = *self.stable_since_ms.get_or_insert(now);
                if now.saturating_sub(since) >= p.recovery_ms {
                    self.level = self.level.saturating_sub(p.step_down);
                    self.stable_since_ms = Some(now);
                    if self.level == 0 {
                        self.boost_started_ms = None;
                        self.state = ControllerState::Normal;
                        return self.decision(bottleneck, "recovered: boost released");
                    }
                    self.state = ControllerState::Recovery;
                    return self.decision(
                        bottleneck,
                        format!("stable: stepping down to {}", self.level),
                    );
                }
                self.state = ControllerState::Recovery;
                return self.decision(bottleneck, "stable; waiting for recovery time");
            }
            self.stable_since_ms = None;
            return self.decision(bottleneck, "between thresholds: holding level");
        }
        self.state = ControllerState::Normal;
        self.decision(bottleneck, "meeting deadlines")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ProfileName, MAX_LEVEL};

    fn stats(miss: f64, consecutive: u32) -> WindowStats {
        WindowStats {
            samples: 240,
            target_period_ns: 16_666_667,
            ewma_interval_ns: Some(16_666_667.0),
            p50_ns: Some(16_666_667),
            p95_ns: Some(20_000_000),
            p99_ns: Some(25_000_000),
            miss_ratio: miss,
            consecutive_misses: consecutive,
            fps: Some(60.0),
            confidence: 1.0,
            activity: Activity::Active,
        }
    }

    fn thermal_ok() -> ThermalLimit {
        ThermalLimit {
            level_cap: MAX_LEVEL,
            suspended: false,
            reason: "ok".into(),
        }
    }

    fn input(now: u64, miss: f64, consecutive: u32, cpu_busy: Option<f64>) -> ControllerInput {
        ControllerInput {
            now_ms: now,
            enabled: true,
            telemetry_ok: true,
            control_fault: false,
            stats: stats(miss, consecutive),
            evidence: LoadEvidence {
                max_thread_busy: cpu_busy,
                gpu_busy: Some(0.5),
                cpu_max_capped: Some(false),
            },
            composition_late: Some(false),
            thermal: thermal_ok(),
        }
    }

    fn balanced() -> Controller {
        Controller::new(ProfileParams::for_profile(ProfileName::Balanced))
    }

    #[test]
    fn isolated_slow_frame_does_not_boost() {
        let mut c = balanced();
        for t in 0..20 {
            let d = c.tick(&input(t * 500, 0.004, 1, Some(0.9)));
            assert_eq!(d.cpu_level, 0, "tick {t}: {d:?}");
        }
    }

    #[test]
    fn sustained_cpu_bound_degradation_steps_up_with_cooldown_and_cap() {
        let mut c = balanced();
        let max = ProfileParams::for_profile(ProfileName::Balanced).max_level;
        let mut levels = vec![];
        for t in 0..200 {
            levels.push(c.tick(&input(t * 500, 0.3, 0, Some(0.95))).cpu_level);
        }
        assert!(levels.iter().all(|l| *l <= max));
        assert_eq!(*levels.iter().max().unwrap(), max);
        // No increase faster than the cooldown (1500 ms = 3 ticks).
        let increases: Vec<_> = levels
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[1] > w[0])
            .map(|(i, _)| i)
            .collect();
        assert!(
            increases.windows(2).all(|w| w[1] - w[0] >= 3),
            "{increases:?}"
        );
    }

    #[test]
    fn gpu_bound_does_not_raise_cpu() {
        let mut c = balanced();
        let mut i = input(0, 0.4, 6, Some(0.4));
        i.evidence.gpu_busy = Some(0.97);
        for t in 0..20 {
            i.now_ms = t * 500;
            let d = c.tick(&i);
            assert_eq!(d.cpu_level, 0);
            assert_eq!(d.bottleneck, Bottleneck::GpuBound);
        }
    }

    #[test]
    fn unknown_bottleneck_is_bounded_conservatively() {
        let mut c = balanced();
        let p = ProfileParams::for_profile(ProfileName::Balanced);
        let mut top = 0;
        for t in 0..200 {
            let mut i = input(t * 500, 0.3, 0, None);
            i.evidence.gpu_busy = None;
            top = top.max(c.tick(&i).cpu_level);
        }
        assert_eq!(top, p.unknown_max_level);
    }

    #[test]
    fn recovery_returns_to_zero() {
        let mut c = balanced();
        let mut t = 0;
        for _ in 0..40 {
            c.tick(&input(t, 0.3, 0, Some(0.95)));
            t += 500;
        }
        let mut last = None;
        for _ in 0..200 {
            last = Some(c.tick(&input(t, 0.0, 0, Some(0.6))));
            t += 500;
        }
        let last = last.unwrap();
        assert_eq!(last.cpu_level, 0);
        assert_eq!(last.state, ControllerState::Normal);
    }

    #[test]
    fn boosts_are_bounded_in_time() {
        let mut p = ProfileParams::for_profile(ProfileName::Balanced);
        p.max_boost_ms = 5_000;
        let mut c = Controller::new(p);
        let mut relaxed = false;
        for t in 0..100 {
            let d = c.tick(&input(t * 500, 0.3, 0, Some(0.95)));
            if d.reason.contains("maximum boost duration") {
                relaxed = true;
            }
        }
        assert!(relaxed);
    }

    #[test]
    fn thermal_suspension_and_cap() {
        let mut c = balanced();
        for t in 0..40 {
            c.tick(&input(t * 500, 0.3, 0, Some(0.95)));
        }
        let mut i = input(20_000, 0.3, 0, Some(0.95));
        i.thermal = ThermalLimit {
            level_cap: 2,
            suspended: false,
            reason: "moderate".into(),
        };
        assert!(c.tick(&i).cpu_level <= 2);
        i.thermal = ThermalLimit {
            level_cap: 0,
            suspended: true,
            reason: "severe".into(),
        };
        let d = c.tick(&i);
        assert_eq!(d.cpu_level, 0);
        assert_eq!(d.state, ControllerState::ThermalLimited);
    }

    #[test]
    fn faults_pause_and_lost_telemetry_release_boosts() {
        for mutate in [
            (|i: &mut ControllerInput| i.control_fault = true) as fn(&mut ControllerInput),
            |i| i.telemetry_ok = false,
            |i| i.stats.activity = Activity::Idle,
            |i| i.enabled = false,
        ] {
            let mut c = balanced();
            for t in 0..40 {
                c.tick(&input(t * 500, 0.3, 0, Some(0.95)));
            }
            let mut i = input(20_000, 0.3, 0, Some(0.95));
            mutate(&mut i);
            assert_eq!(c.tick(&i).cpu_level, 0);
        }
    }

    #[test]
    fn burst_gives_short_boost_that_expires() {
        let mut c = balanced();
        let d = c.tick(&input(0, 0.05, 5, Some(0.95)));
        assert_eq!(d.state, ControllerState::BoostShort);
        assert!(d.cpu_level > 0);
        let d = c.tick(&input(2_000, 0.0, 0, Some(0.95)));
        assert_eq!(d.cpu_level, 0);
    }

    #[test]
    fn adaptive_reverts_ineffective_steps() {
        let mut c = Controller::new(ProfileParams::for_profile(ProfileName::Adaptive));
        let mut reverted = false;
        for t in 0..60 {
            // Misses never improve whatever the level: boosting is useless here.
            let d = c.tick(&input(t * 500, 0.3, 0, Some(0.95)));
            reverted |= d.reason.contains("did not reduce misses");
        }
        assert!(reverted);
    }

    #[test]
    fn low_confidence_does_not_act() {
        let mut c = balanced();
        for t in 0..20 {
            let mut i = input(t * 500, 0.5, 0, Some(0.95));
            i.stats.confidence = 0.3;
            assert_eq!(c.tick(&i).cpu_level, 0);
        }
    }

    #[test]
    fn classifier_cases() {
        let e = |c, g, cap| LoadEvidence {
            max_thread_busy: c,
            gpu_busy: g,
            cpu_max_capped: cap,
        };
        assert_eq!(
            classify(&e(Some(0.9), Some(0.5), Some(false)), None),
            Bottleneck::CpuBound
        );
        assert_eq!(
            classify(&e(Some(0.5), Some(0.95), None), None),
            Bottleneck::GpuBound
        );
        assert_eq!(
            classify(&e(Some(0.3), Some(0.3), None), None),
            Bottleneck::AppOrSyncWait
        );
        assert_eq!(
            classify(&e(Some(0.9), None, Some(true)), None),
            Bottleneck::ThermalOrPower
        );
        assert_eq!(
            classify(&e(None, None, None), Some(true)),
            Bottleneck::Composition
        );
        assert_eq!(classify(&e(None, None, None), None), Bottleneck::Unknown);
    }
}
