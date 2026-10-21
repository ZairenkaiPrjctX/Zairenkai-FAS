// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! Session diagnostics: what the controller did and on what evidence, without app data.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::controller::{ControllerState, Decision};
use crate::model::TelemetrySource;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionDiag {
    pub ticks: u64,
    pub state_ms: BTreeMap<String, u64>,
    pub transitions: u64,
    /// Ticks on which the CPU level rose.
    pub boost_steps: u64,
    pub max_cpu_level: u8,
    /// Integral of level over time (level·ms), a proxy for how much boosting happened.
    pub level_ms: u64,
    pub control_errors: u64,
    pub telemetry_source: Option<TelemetrySource>,
    /// Frames accepted by the analyzer (measured).
    pub frames: u64,
    /// Frames over the deadline (measured against the selected target).
    pub missed_frames: u64,
    #[serde(skip)]
    last: Option<(u64, ControllerState, u8)>,
}

fn state_name(s: ControllerState) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

impl SessionDiag {
    pub fn record(&mut self, now_ms: u64, d: &Decision) {
        self.ticks += 1;
        if let Some((t, s, level)) = self.last {
            let dt = now_ms.saturating_sub(t);
            *self.state_ms.entry(state_name(s)).or_default() += dt;
            self.level_ms += dt * level as u64;
            if s != d.state {
                self.transitions += 1;
            }
            if d.cpu_level > level {
                self.boost_steps += 1;
            }
        }
        self.max_cpu_level = self.max_cpu_level.max(d.cpu_level);
        self.last = Some((now_ms, d.state, d.cpu_level));
    }

    pub fn record_frames(&mut self, accepted: u64, missed: u64) {
        self.frames += accepted;
        self.missed_frames += missed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::Bottleneck;

    fn d(state: ControllerState, level: u8) -> Decision {
        Decision {
            state,
            cpu_level: level,
            gpu_level: 0,
            bottleneck: Bottleneck::Unknown,
            reason: String::new(),
        }
    }

    #[test]
    fn durations_and_counts() {
        let mut s = SessionDiag::default();
        s.record(0, &d(ControllerState::Normal, 0));
        s.record(500, &d(ControllerState::BoostSustained, 2));
        s.record(1_500, &d(ControllerState::BoostSustained, 3));
        s.record(2_000, &d(ControllerState::Normal, 0));
        assert_eq!(s.state_ms["normal"], 500);
        assert_eq!(s.state_ms["boost_sustained"], 1_500);
        assert_eq!(s.transitions, 2);
        assert_eq!(s.boost_steps, 2);
        assert_eq!(s.max_cpu_level, 3);
        assert_eq!(s.level_ms, 2 * 1_000 + 3 * 500);
    }
}
