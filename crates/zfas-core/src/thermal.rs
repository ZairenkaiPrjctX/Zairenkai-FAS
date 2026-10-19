//! Thermal-aware limits.
//!
//! The primary input is Android's thermal status (Thermal HAL / `ThermalService`), whose
//! thresholds are defined by the device vendor. Raw sensor temperatures are used only with
//! thresholds the user or a validated device profile configured; AFID ships no universal
//! temperature thresholds. Missing or unreadable thermal data is treated conservatively.

use serde::{Deserialize, Serialize};

use crate::policy::MAX_LEVEL;

/// `android.os.PowerManager` thermal status values (THERMAL_STATUS_*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThermalStatus {
    None = 0,
    Light = 1,
    Moderate = 2,
    Severe = 3,
    Critical = 4,
    Emergency = 5,
    Shutdown = 6,
}

impl ThermalStatus {
    pub fn from_code(c: i64) -> Option<Self> {
        Some(match c {
            0 => Self::None,
            1 => Self::Light,
            2 => Self::Moderate,
            3 => Self::Severe,
            4 => Self::Critical,
            5 => Self::Emergency,
            6 => Self::Shutdown,
            _ => return None,
        })
    }
}

/// A sensor reading with a configured (never built-in) threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneReading {
    pub name: String,
    pub millicelsius: Option<i64>,
    /// Configured derate threshold for this zone, if any.
    pub derate_mc: Option<i64>,
    /// Configured suspend threshold for this zone, if any.
    pub suspend_mc: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThermalInput {
    /// `None` when no thermal status is available on this device.
    pub status: Option<ThermalStatus>,
    pub zones: Vec<ZoneReading>,
    /// Thermal data could not be read this tick (errors, timeouts).
    pub read_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ThermalPolicy {
    /// Highest boost level allowed per status (index = status code). Levels above the
    /// profile's own maximum are ignored.
    pub level_cap_by_status: [u8; 7],
    /// With no thermal data at all, cap boosts at this level (conservative).
    pub unknown_cap: u8,
    /// Cooling time before a lifted cap is restored one status step.
    pub recovery_ms: u64,
}

impl Default for ThermalPolicy {
    fn default() -> Self {
        Self {
            // NONE, LIGHT, MODERATE: full, reduced, small. SEVERE and above: no boosting.
            level_cap_by_status: [MAX_LEVEL, 6, 3, 0, 0, 0, 0],
            unknown_cap: 4,
            recovery_ms: 10_000,
        }
    }
}

impl ThermalPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.level_cap_by_status.iter().any(|v| *v > MAX_LEVEL) || self.unknown_cap > MAX_LEVEL {
            return Err(format!("thermal level caps must be <= {MAX_LEVEL}"));
        }
        if self.level_cap_by_status.windows(2).any(|w| w[1] > w[0]) {
            return Err("thermal caps must not increase with hotter status".into());
        }
        if self.level_cap_by_status[3..].iter().any(|v| *v != 0) {
            return Err("SEVERE and hotter must suspend boosting (cap 0)".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThermalLimit {
    pub level_cap: u8,
    /// No boosting at all; outstanding boosts must be released immediately.
    pub suspended: bool,
    pub reason: String,
}

/// Turns thermal input into a level cap with gradual recovery: the cap drops immediately
/// when it gets hotter, but rises again only after `recovery_ms` without a worse reading.
#[derive(Debug, Clone)]
pub struct ThermalGovernor {
    policy: ThermalPolicy,
    effective_cap: u8,
    last_worsening_ms: Option<u64>,
}

impl ThermalGovernor {
    pub fn new(policy: ThermalPolicy) -> Self {
        Self {
            policy,
            effective_cap: policy.unknown_cap,
            last_worsening_ms: None,
        }
    }

    pub fn update(&mut self, now_ms: u64, input: &ThermalInput) -> ThermalLimit {
        let mut reasons = Vec::new();
        let mut cap = match input.status {
            Some(s) => {
                if s > ThermalStatus::None {
                    reasons.push(format!("thermal status {s:?}"));
                }
                self.policy.level_cap_by_status[s as usize]
            }
            None => {
                reasons.push("no thermal status available; conservative cap".to_string());
                self.policy.unknown_cap
            }
        };
        if input.read_error {
            cap = cap.min(self.policy.unknown_cap);
            reasons.push("thermal read error".to_string());
        }
        let mut suspended = cap == 0;
        for z in &input.zones {
            let Some(t) = z.millicelsius else { continue };
            if z.suspend_mc.is_some_and(|lim| t >= lim) {
                suspended = true;
                cap = 0;
                reasons.push(format!(
                    "{} {:.1}°C >= configured suspend limit",
                    z.name,
                    t as f64 / 1000.0
                ));
            } else if z.derate_mc.is_some_and(|lim| t >= lim) {
                cap = cap.min(MAX_LEVEL / 2);
                reasons.push(format!(
                    "{} {:.1}°C >= configured derate limit",
                    z.name,
                    t as f64 / 1000.0
                ));
            }
        }
        if cap < self.effective_cap {
            self.effective_cap = cap;
            self.last_worsening_ms = Some(now_ms);
        } else if cap > self.effective_cap {
            let cooled = self
                .last_worsening_ms
                .is_none_or(|t| now_ms.saturating_sub(t) >= self.policy.recovery_ms);
            if cooled {
                // Recover one step at a time.
                self.effective_cap = (self.effective_cap + 2).min(cap);
                self.last_worsening_ms = Some(now_ms);
                reasons.push("recovering after cooling".to_string());
            } else {
                reasons.push("waiting for cooling before lifting the cap".to_string());
            }
        }
        if suspended {
            self.effective_cap = 0;
        }
        ThermalLimit {
            level_cap: self.effective_cap,
            suspended: suspended || self.effective_cap == 0,
            reason: if reasons.is_empty() {
                "no thermal restriction".into()
            } else {
                reasons.join("; ")
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(s: Option<ThermalStatus>) -> ThermalInput {
        ThermalInput {
            status: s,
            zones: vec![],
            read_error: false,
        }
    }

    #[test]
    fn severe_suspends_and_recovery_is_gradual() {
        let mut g = ThermalGovernor::new(ThermalPolicy::default());
        assert_eq!(
            g.update(0, &input(Some(ThermalStatus::None))).level_cap,
            MAX_LEVEL.min(6)
        );
        let l = g.update(1_000, &input(Some(ThermalStatus::Severe)));
        assert!(l.suspended);
        assert_eq!(l.level_cap, 0);
        // Cooled down, but within recovery time: still capped.
        assert_eq!(
            g.update(5_000, &input(Some(ThermalStatus::None))).level_cap,
            0
        );
        // After recovery time: one step up, not straight to the maximum.
        let l = g.update(11_500, &input(Some(ThermalStatus::None)));
        assert_eq!(l.level_cap, 2);
        assert!(!l.suspended);
    }

    #[test]
    fn unknown_status_is_conservative() {
        let mut g = ThermalGovernor::new(ThermalPolicy::default());
        let l = g.update(0, &input(None));
        assert_eq!(l.level_cap, ThermalPolicy::default().unknown_cap);
    }

    #[test]
    fn configured_zone_suspend() {
        let mut g = ThermalGovernor::new(ThermalPolicy::default());
        let inp = ThermalInput {
            status: Some(ThermalStatus::None),
            zones: vec![ZoneReading {
                name: "cpu-big".into(),
                millicelsius: Some(91_000),
                derate_mc: Some(80_000),
                suspend_mc: Some(90_000),
            }],
            read_error: false,
        };
        assert!(g.update(0, &inp).suspended);
    }

    #[test]
    fn policy_must_suspend_when_severe() {
        let mut p = ThermalPolicy::default();
        p.level_cap_by_status[3] = 2;
        assert!(p.validate().is_err());
        assert!(ThermalPolicy::default().validate().is_ok());
    }
}
