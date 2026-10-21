// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! Builds one [`StepInput`] per tick from the live platform.

use zfas_core::cpcs::SchedEvent;
use zfas_core::engine::StepInput;
use zfas_core::model::{Clock, LoadEvidence, Timestamp};
use zfas_core::thermal::ThermalInput;
use zfas_platform::cpufreq::{self, Policy};
use zfas_platform::exec::Runner;
use zfas_platform::foreground;
use zfas_platform::gpu;
use zfas_platform::procstat::{clock_ticks_per_sec, BusySampler};
use zfas_platform::surfaceflinger::{composition_late_ratio, SfLatencySource};
use zfas_platform::sysroot::SysRoot;
use zfas_platform::thermal::ThermalReader;
use zfas_platform::tracefs::TraceSession;

use crate::util::monotonic_ns;

pub struct Observer<'a> {
    pub root: SysRoot,
    runner: &'a dyn Runner,
    sf: SfLatencySource<'a>,
    busy: BusySampler,
    hz: u64,
    thermal: ThermalReader,
    last_thermal: Option<ThermalInput>,
    policies: Vec<Policy>,
    pub trace: Option<TraceSession>,
    pub package: Option<String>,
    pub pid: Option<u32>,
    ticks: u64,
    pub last_telemetry_error: Option<String>,
}

impl<'a> Observer<'a> {
    pub fn new(
        root: SysRoot,
        runner: &'a dyn Runner,
        thermal: ThermalReader,
        policies: Vec<Policy>,
    ) -> Self {
        Self {
            root,
            runner,
            sf: SfLatencySource::new(runner),
            busy: BusySampler::default(),
            hz: clock_ticks_per_sec(),
            thermal,
            last_thermal: None,
            policies,
            trace: None,
            package: None,
            pid: None,
            ticks: 0,
            last_telemetry_error: None,
        }
    }

    pub fn layer(&self) -> Option<&str> {
        self.sf.layer()
    }

    /// Refresh the foreground app. Returns `true` when it changed.
    pub fn refresh_foreground(&mut self) -> bool {
        let pkg = foreground::foreground_package(self.runner).ok().flatten();
        if pkg == self.package {
            if let (Some(p), None) = (&self.package, self.pid) {
                self.pid = foreground::pid_of(&self.root, p);
            }
            return false;
        }
        self.package = pkg;
        self.pid = self
            .package
            .as_deref()
            .and_then(|p| foreground::pid_of(&self.root, p));
        self.sf.forget_layer();
        true
    }

    pub fn target_tids(&self) -> Vec<u32> {
        self.pid
            .map(|p| foreground::tids_of(&self.root, p))
            .unwrap_or_default()
    }

    pub fn observe(&mut self) -> StepInput {
        self.ticks += 1;
        let (frames, refresh, telemetry_ok, composition_late) = match self.package.clone() {
            None => (vec![], None, false, None),
            Some(pkg) => match self.sf.poll(&pkg) {
                Ok(d) => {
                    self.last_telemetry_error = None;
                    let late = d
                        .refresh_period_ns
                        .and_then(|p| composition_late_ratio(&d.frames, p))
                        .map(|r| r >= 0.3);
                    (d.frames, d.refresh_period_ns, true, late)
                }
                Err(e) => {
                    self.last_telemetry_error = Some(e.to_string());
                    (vec![], None, false, None)
                }
            },
        };
        let now = monotonic_ns();
        let max_thread_busy = self
            .pid
            .and_then(|pid| self.busy.sample(&self.root, pid, now / 1_000_000, self.hz));
        let cpu_max_capped = if self.policies.is_empty() {
            None
        } else {
            Some(self.policies.iter().any(|p| {
                cpufreq::read_policy(&self.root, &p.name)
                    .map(|now| now.max_capped())
                    .unwrap_or(false)
            }))
        };
        // Thermal status via dumpsys every other tick; reuse the previous reading in between.
        if self.ticks % 2 == 1 || self.last_thermal.is_none() {
            self.last_thermal = Some(self.thermal.read(&self.root, self.runner));
        }
        let sched: Vec<SchedEvent> = self
            .trace
            .as_mut()
            .map(|t| t.drain(8 << 20))
            .unwrap_or_default();
        StepInput {
            now: Timestamp::new(Clock::Monotonic, now),
            frames,
            refresh_period_ns: refresh,
            telemetry_ok,
            evidence: LoadEvidence {
                max_thread_busy,
                gpu_busy: gpu::gpu_busy(&self.root),
                cpu_max_capped,
            },
            composition_late,
            thermal: self.last_thermal.clone().expect("set above"),
            sched,
        }
    }
}
