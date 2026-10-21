// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! The FAS engine: one deterministic step from observations to applied control.
//!
//! The daemon and the replay tool both drive this type, so a recorded session replays through
//! exactly the code that runs on the device; only the backend differs (cpufreq vs. memory).

use serde::{Deserialize, Serialize};

use crate::analyzer::{Activity, FrameAnalyzer, WindowStats};
use crate::config::{AppSettings, Config};
use crate::control::{ControlBackend, DomainInfo, DomainKind, LeaseManager};
use crate::controller::{Controller, ControllerInput, Decision};
use crate::cpcs::{scale_levels, CpcsAnalyzer, CpcsOutput, SchedEvent};
use crate::diag::SessionDiag;
use crate::model::{Clock, FrameEvent, LoadEvidence, Timestamp};
use crate::target::{candidates, TargetSelector};
use crate::thermal::{ThermalGovernor, ThermalInput, ThermalLimit};

pub const OWNER: &str = "controller";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepInput {
    /// Current time in the telemetry clock domain.
    pub now: Timestamp,
    pub frames: Vec<FrameEvent>,
    pub refresh_period_ns: Option<u64>,
    /// The telemetry backend answered this tick.
    pub telemetry_ok: bool,
    pub evidence: LoadEvidence,
    pub composition_late: Option<bool>,
    pub thermal: ThermalInput,
    #[serde(default)]
    pub sched: Vec<SchedEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepOutput {
    pub decision: Decision,
    pub stats: WindowStats,
    pub target_fps: Option<u32>,
    pub thermal: ThermalLimit,
    /// Level in effect per control domain after this step.
    pub levels: Vec<u8>,
    pub cpcs: Option<CpcsOutput>,
    pub control_error: Option<String>,
}

pub struct Engine {
    cfg: Config,
    app: AppSettings,
    domains: Vec<DomainInfo>,
    analyzer: FrameAnalyzer,
    selector: TargetSelector,
    controller: Controller,
    thermal: ThermalGovernor,
    leases: LeaseManager,
    cpcs: Option<CpcsAnalyzer>,
    pub diag: SessionDiag,
    fault_until_ms: Option<u64>,
    last_target_eval_ms: Option<u64>,
    ticks: u64,
}

fn cpu_domain_map(domains: &[DomainInfo]) -> Vec<Option<usize>> {
    let max_cpu = domains
        .iter()
        .flat_map(|d| d.cpus.iter().copied())
        .max()
        .map_or(0, |m| m as usize + 1);
    let mut map = vec![None; max_cpu];
    for (i, d) in domains
        .iter()
        .enumerate()
        .filter(|(_, d)| d.kind == DomainKind::Cpu)
    {
        for c in &d.cpus {
            map[*c as usize] = Some(i);
        }
    }
    map
}

impl Engine {
    pub fn new(cfg: Config, app: AppSettings, domains: Vec<DomainInfo>, clock: Clock) -> Self {
        let lease_ms = cfg.general.tick_ms * 3;
        let cpcs = cfg
            .cpcs
            .enabled
            .then(|| CpcsAnalyzer::new(cfg.cpcs, clock, cpu_domain_map(&domains), domains.len()));
        Self {
            analyzer: FrameAnalyzer::new(cfg.analyzer, 60.0),
            selector: TargetSelector::new(cfg.target),
            controller: Controller::new(app.params),
            thermal: ThermalGovernor::new(cfg.thermal.policy()),
            leases: LeaseManager::new(domains.len(), lease_ms),
            cpcs,
            diag: SessionDiag::default(),
            fault_until_ms: None,
            last_target_eval_ms: None,
            ticks: 0,
            cfg,
            app,
            domains,
        }
    }

    pub fn app(&self) -> &AppSettings {
        &self.app
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn domains(&self) -> &[DomainInfo] {
        &self.domains
    }

    /// Foreground app changed (or its settings did): forget all history and boosts.
    pub fn set_app(
        &mut self,
        app: AppSettings,
        backend: &mut dyn ControlBackend,
    ) -> Result<(), String> {
        self.app = app;
        self.controller.set_params(self.app.params);
        self.controller.reset();
        self.analyzer.reset();
        self.selector.reset();
        self.last_target_eval_ms = None;
        if let Some(c) = self.cpcs.as_mut() {
            c.reset();
        }
        self.leases.restore(backend).map_err(|e| e.to_string())
    }

    /// Replace the configuration (reload). Boosts are released and the engine restarts with the
    /// new settings; only the session diagnostics carry over.
    pub fn set_config(
        &mut self,
        cfg: Config,
        app: AppSettings,
        backend: &mut dyn ControlBackend,
    ) -> Result<(), String> {
        let clock = Clock::Monotonic;
        let diag = std::mem::take(&mut self.diag);
        let r = self.leases.restore(backend).map_err(|e| e.to_string());
        *self = Engine::new(cfg, app, self.domains.clone(), clock);
        self.diag = diag;
        r
    }

    pub fn set_cpcs_targets(&mut self, tids: &[u32]) {
        if let Some(c) = self.cpcs.as_mut() {
            c.set_targets(tids.iter().copied());
        }
    }

    pub fn cpcs_enabled(&self) -> bool {
        self.cpcs.is_some()
    }

    pub fn step(&mut self, input: StepInput, backend: &mut dyn ControlBackend) -> StepOutput {
        self.ticks += 1;
        let now_ms = input.now.ns / 1_000_000;
        let mut frames = input.frames;
        frames.sort_by_key(|f| f.present.ns);
        self.analyzer.ingest(&frames);
        let mut stats = self.analyzer.stats(input.now);

        // Target selection about once per second.
        let due = self
            .last_target_eval_ms
            .is_none_or(|t| now_ms.saturating_sub(t) >= 1_000);
        if due && stats.activity != Activity::Idle && stats.samples >= self.cfg.analyzer.min_samples
        {
            self.last_target_eval_ms = Some(now_ms);
            let refresh = input
                .refresh_period_ns
                .filter(|p| *p > 0)
                .map(|p| 1e9 / p as f64);
            let cands = candidates(&self.app.target_fps, refresh);
            let before = self.selector.current();
            if let Some(t) = self.selector.update(stats.fps, &cands) {
                if before != Some(t) {
                    self.analyzer.set_target_fps(t as f64);
                    stats = self.analyzer.stats(input.now);
                }
            }
        }

        let thermal = self.thermal.update(now_ms, &input.thermal);
        let control_fault = self.fault_until_ms.is_some_and(|t| now_ms < t);
        let decision = self.controller.tick(&ControllerInput {
            now_ms,
            enabled: self.app.enabled,
            telemetry_ok: input.telemetry_ok,
            control_fault,
            stats,
            evidence: input.evidence,
            composition_late: input.composition_late,
            thermal: thermal.clone(),
        });

        let cpcs = self.cpcs.as_mut().map(|c| {
            c.ingest(&input.sched);
            let bounds: Vec<Timestamp> = frames.iter().map(|f| f.present).collect();
            c.observe_frames(&bounds);
            c.output()
        });
        let weights = cpcs
            .as_ref()
            .map(|o| o.weights.clone())
            .unwrap_or_else(|| vec![1.0; self.domains.len()]);
        let mut per_domain = scale_levels(decision.cpu_level, &weights);
        for (i, d) in self.domains.iter().enumerate() {
            if d.kind != DomainKind::Cpu {
                per_domain[i] = 0;
            }
        }

        let lease_ms = self.cfg.general.tick_ms * 3;
        for (i, l) in per_domain.iter().enumerate() {
            self.leases.grant(OWNER, i, *l, now_ms, lease_ms);
        }
        let caps = vec![thermal.level_cap; self.domains.len()];
        let mut control_error = None;
        match self.leases.sync(now_ms, &caps, backend) {
            Ok(_) => {
                if self.ticks % self.cfg.control.verify_every_ticks as u64 == 0 {
                    if let Err(e) = self.leases.verify(backend) {
                        control_error = Some(e.to_string());
                    }
                }
            }
            Err(e) => control_error = Some(e.to_string()),
        }
        if control_error.is_some() {
            self.fault_until_ms = Some(now_ms + self.cfg.control.fault_backoff_ms);
            self.diag.control_errors += 1;
        }
        self.diag.record(now_ms, &decision);
        self.diag.frames = self.analyzer.quality.accepted;
        self.diag.missed_frames = self.analyzer.quality.missed;
        if let Some(f) = frames.first() {
            self.diag.telemetry_source = Some(f.source);
        }
        StepOutput {
            decision,
            stats,
            target_fps: self.selector.current(),
            thermal,
            levels: self.leases.applied().to_vec(),
            cpcs,
            control_error,
        }
    }

    /// Release every boost and restore original settings.
    pub fn shutdown(&mut self, backend: &mut dyn ControlBackend) -> Result<(), String> {
        self.leases.restore(backend).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::control::MemoryBackend;
    use crate::controller::ControllerState;
    use crate::model::TelemetrySource;
    use crate::thermal::ThermalStatus;

    pub fn domains() -> Vec<DomainInfo> {
        vec![
            DomainInfo {
                id: "cpufreq/policy0".into(),
                kind: DomainKind::Cpu,
                cpus: vec![0, 1, 2, 3],
                capacity_khz: 1_800_000,
            },
            DomainInfo {
                id: "cpufreq/policy4".into(),
                kind: DomainKind::Cpu,
                cpus: vec![4, 5, 6, 7],
                capacity_khz: 2_400_000,
            },
        ]
    }

    /// Feed `secs` of frames at `fps` presented on a 120 Hz panel; ticks every 500 ms.
    fn run(
        e: &mut Engine,
        b: &mut MemoryBackend,
        start_ns: u64,
        secs: u64,
        fps: f64,
        busy: f64,
        status: ThermalStatus,
    ) -> (u64, Vec<StepOutput>) {
        let period = (1e9 / fps) as u64;
        let mut t = start_ns;
        let mut next_frame = start_ns;
        let mut outs = vec![];
        let end = start_ns + secs * 1_000_000_000;
        while t < end {
            t += 500_000_000;
            let mut frames = vec![];
            while next_frame <= t {
                frames.push(FrameEvent {
                    source: TelemetrySource::Synthetic,
                    present: Timestamp::new(Clock::Monotonic, next_frame),
                    ready: None,
                    desired_present: None,
                });
                next_frame += period;
            }
            outs.push(e.step(
                StepInput {
                    now: Timestamp::new(Clock::Monotonic, t),
                    frames,
                    refresh_period_ns: Some(8_333_333),
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
                },
                b,
            ));
        }
        (t, outs)
    }

    fn engine(app_fps: Vec<u32>) -> (Engine, MemoryBackend) {
        let cfg = Config::default();
        let mut app = cfg.for_app(None);
        app.target_fps = app_fps;
        (
            Engine::new(cfg, app, domains(), Clock::Monotonic),
            MemoryBackend::new(domains()),
        )
    }

    #[test]
    fn steady_game_is_left_alone() {
        let (mut e, mut b) = engine(vec![]);
        let (_, outs) = run(
            &mut e,
            &mut b,
            1_000_000_000,
            20,
            60.0,
            0.6,
            ThermalStatus::None,
        );
        let last = outs.last().unwrap();
        assert_eq!(last.target_fps, Some(60));
        assert_eq!(last.decision.state, ControllerState::Normal);
        assert!(outs.iter().all(|o| o.levels.iter().all(|l| *l == 0)));
        assert_eq!(b.writes, 0);
    }

    #[test]
    fn cpu_bound_drop_boosts_then_releases_on_pause_and_shutdown() {
        // The app is configured for 60 FPS but renders at 50 FPS, CPU bound.
        let (mut e, mut b) = engine(vec![60]);
        let (t, outs) = run(
            &mut e,
            &mut b,
            1_000_000_000,
            30,
            50.0,
            0.95,
            ThermalStatus::None,
        );
        assert!(
            outs.iter().any(|o| o.levels.iter().any(|l| *l > 0)),
            "should boost"
        );
        let max = ProfileMax::balanced();
        assert!(outs.iter().all(|o| o.levels.iter().all(|l| *l <= max)));
        // Rendering stops: no frames for several seconds.
        let mut last = None;
        for i in 1..8 {
            last = Some(e.step(
                StepInput {
                    now: Timestamp::new(Clock::Monotonic, t + i * 500_000_000),
                    frames: vec![],
                    refresh_period_ns: Some(8_333_333),
                    telemetry_ok: true,
                    evidence: LoadEvidence::default(),
                    composition_late: None,
                    thermal: ThermalInput {
                        status: Some(ThermalStatus::None),
                        zones: vec![],
                        read_error: false,
                    },
                    sched: vec![],
                },
                &mut b,
            ));
        }
        let last = last.unwrap();
        assert_eq!(last.decision.state, ControllerState::Paused);
        assert_eq!(b.levels, vec![0, 0]);
        e.shutdown(&mut b).unwrap();
        assert_eq!(b.levels, vec![0, 0]);
    }

    #[test]
    fn severe_thermal_releases_immediately() {
        let (mut e, mut b) = engine(vec![60]);
        let (t, _) = run(
            &mut e,
            &mut b,
            1_000_000_000,
            30,
            50.0,
            0.95,
            ThermalStatus::None,
        );
        assert!(b.levels.iter().any(|l| *l > 0));
        let (_, outs) = run(&mut e, &mut b, t, 1, 50.0, 0.95, ThermalStatus::Severe);
        assert_eq!(outs[0].decision.state, ControllerState::ThermalLimited);
        assert_eq!(b.levels, vec![0, 0]);
    }

    #[test]
    fn control_fault_backs_off() {
        let (mut e, mut b) = engine(vec![60]);
        b.fail_domain = Some(1);
        let (_, outs) = run(
            &mut e,
            &mut b,
            1_000_000_000,
            30,
            50.0,
            0.95,
            ThermalStatus::None,
        );
        let first_err = outs
            .iter()
            .position(|o| o.control_error.is_some())
            .expect("fault seen");
        assert_eq!(
            outs[first_err + 1].decision.state,
            ControllerState::SafeFallback
        );
        assert_eq!(b.levels, vec![0, 0]);
        assert!(e.diag.control_errors >= 1);
    }

    struct ProfileMax;
    impl ProfileMax {
        fn balanced() -> u8 {
            crate::policy::ProfileParams::for_profile(crate::policy::ProfileName::Balanced)
                .max_level
        }
    }
}
