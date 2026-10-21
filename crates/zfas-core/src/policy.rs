// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! Performance profiles.
//!
//! A profile never contains absolute frequencies. It works in *boost levels* 0..=10, where a
//! level is a fraction of each CPU policy's validated range (see `zfas-platform::cpufreq`),
//! so the same profile means the same thing on different SoCs.

use serde::{Deserialize, Serialize};

/// Highest boost level. Level `n` maps to `n / MAX_LEVEL` of a policy's frequency range.
pub const MAX_LEVEL: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileName {
    PowerSaver,
    Balanced,
    Performance,
    FastResponse,
    Adaptive,
}

impl ProfileName {
    pub const ALL: [ProfileName; 5] = [
        ProfileName::PowerSaver,
        ProfileName::Balanced,
        ProfileName::Performance,
        ProfileName::FastResponse,
        ProfileName::Adaptive,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ProfileName::PowerSaver => "power_saver",
            ProfileName::Balanced => "balanced",
            ProfileName::Performance => "performance",
            ProfileName::FastResponse => "fast_response",
            ProfileName::Adaptive => "adaptive",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

/// Controller parameters of one profile. Durations in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProfileParams {
    /// Highest boost level this profile may request (<= MAX_LEVEL).
    pub max_level: u8,
    /// Highest level when the bottleneck is unknown (conservative bounded policy).
    pub unknown_max_level: u8,
    /// Level of a short boost on a burst of consecutive misses.
    pub short_level: u8,
    pub step_up: u8,
    pub step_down: u8,
    /// Enter degraded when the window miss ratio is at least this.
    pub degrade_miss_ratio: f64,
    /// Count as recovered when the miss ratio is at most this (hysteresis).
    pub recover_miss_ratio: f64,
    /// Consecutive missed deadlines that trigger a short boost.
    pub consecutive_trigger: u32,
    /// Controller ticks a degradation must persist before a sustained step up.
    pub eval_ticks: u32,
    /// Minimum time between two level increases.
    pub cooldown_ms: u64,
    pub short_boost_ms: u64,
    /// A sustained boost is relaxed by one step after this long, then re-evaluated, so no
    /// boost becomes permanent.
    pub max_boost_ms: u64,
    /// Stable time before stepping down.
    pub recovery_ms: u64,
    /// Minimum analyzer confidence to act at all.
    pub min_confidence: f64,
    /// Learn whether a step helped and undo steps that did not (ADAPTIVE).
    pub track_effectiveness: bool,
}

impl ProfileParams {
    pub fn for_profile(p: ProfileName) -> Self {
        let base = ProfileParams {
            max_level: 6,
            unknown_max_level: 3,
            short_level: 3,
            step_up: 1,
            step_down: 1,
            degrade_miss_ratio: 0.10,
            recover_miss_ratio: 0.03,
            consecutive_trigger: 4,
            eval_ticks: 3,
            cooldown_ms: 1_500,
            short_boost_ms: 1_000,
            max_boost_ms: 20_000,
            recovery_ms: 4_000,
            min_confidence: 0.8,
            track_effectiveness: false,
        };
        match p {
            ProfileName::Balanced => base,
            ProfileName::PowerSaver => ProfileParams {
                max_level: 3,
                unknown_max_level: 1,
                short_level: 2,
                degrade_miss_ratio: 0.20,
                recover_miss_ratio: 0.08,
                consecutive_trigger: 8,
                eval_ticks: 5,
                cooldown_ms: 3_000,
                max_boost_ms: 10_000,
                recovery_ms: 2_000,
                ..base
            },
            ProfileName::Performance => ProfileParams {
                max_level: 9,
                unknown_max_level: 5,
                short_level: 5,
                degrade_miss_ratio: 0.05,
                recover_miss_ratio: 0.01,
                consecutive_trigger: 3,
                eval_ticks: 2,
                cooldown_ms: 1_000,
                max_boost_ms: 30_000,
                recovery_ms: 6_000,
                ..base
            },
            ProfileName::FastResponse => ProfileParams {
                max_level: 8,
                unknown_max_level: 4,
                short_level: 6,
                step_up: 2,
                consecutive_trigger: 2,
                eval_ticks: 1,
                cooldown_ms: 500,
                short_boost_ms: 1_500,
                recovery_ms: 3_000,
                ..base
            },
            ProfileName::Adaptive => ProfileParams {
                max_level: 8,
                unknown_max_level: 4,
                track_effectiveness: true,
                ..base
            },
        }
    }

    /// Reject parameter sets that would make the controller unsafe or meaningless.
    pub fn validate(&self) -> Result<(), String> {
        let lvl = |v: u8, n: &str| {
            if v > MAX_LEVEL {
                Err(format!("{n} {v} exceeds {MAX_LEVEL}"))
            } else {
                Ok(())
            }
        };
        lvl(self.max_level, "max_level")?;
        lvl(self.unknown_max_level, "unknown_max_level")?;
        lvl(self.short_level, "short_level")?;
        if self.unknown_max_level > self.max_level || self.short_level > self.max_level {
            return Err("unknown_max_level and short_level must not exceed max_level".into());
        }
        if self.step_up == 0 || self.step_down == 0 {
            return Err("step_up and step_down must be at least 1".into());
        }
        let ratio = |v: f64, n: &str| {
            if (0.0..=1.0).contains(&v) {
                Ok(())
            } else {
                Err(format!("{n} must be within 0..=1"))
            }
        };
        ratio(self.degrade_miss_ratio, "degrade_miss_ratio")?;
        ratio(self.recover_miss_ratio, "recover_miss_ratio")?;
        ratio(self.min_confidence, "min_confidence")?;
        if self.recover_miss_ratio >= self.degrade_miss_ratio {
            return Err("recover_miss_ratio must be below degrade_miss_ratio (hysteresis)".into());
        }
        if self.consecutive_trigger < 2 {
            return Err(
                "consecutive_trigger must be at least 2 (one slow frame is not evidence)".into(),
            );
        }
        if self.eval_ticks == 0 {
            return Err("eval_ticks must be at least 1".into());
        }
        if self.max_boost_ms < self.short_boost_ms || self.max_boost_ms > 120_000 {
            return Err("max_boost_ms must be >= short_boost_ms and <= 120000".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_profiles_validate() {
        for p in ProfileName::ALL {
            ProfileParams::for_profile(p).validate().unwrap();
            assert_eq!(ProfileName::parse(p.as_str()), Some(p));
        }
    }

    #[test]
    fn hysteresis_and_single_frame_guards() {
        let mut p = ProfileParams::for_profile(ProfileName::Balanced);
        p.recover_miss_ratio = p.degrade_miss_ratio;
        assert!(p.validate().is_err());
        let mut p = ProfileParams::for_profile(ProfileName::Balanced);
        p.consecutive_trigger = 1;
        assert!(p.validate().is_err());
    }
}
