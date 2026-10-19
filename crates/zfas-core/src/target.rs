//! Target frame-rate selection.
//!
//! The target is the lowest candidate rate the app is plausibly trying to reach, chosen from
//! the per-app list (or the default ladder) and never above the display refresh rate. A switch
//! needs several consecutive agreeing evaluations, more for switching down than up, so a
//! single stutter does not lower the target and a loading screen does not raise it.

use serde::{Deserialize, Serialize};

/// Candidate rates used when an app has no configured list.
pub const DEFAULT_LADDER: [u32; 6] = [30, 45, 60, 90, 120, 144];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetConfig {
    /// Observed FPS may be this far above a candidate and still count as "aiming for it".
    pub margin_fps: f64,
    /// Consecutive evaluations needed to raise the target.
    pub up_after: u32,
    /// Consecutive evaluations needed to lower the target.
    pub down_after: u32,
}

impl Default for TargetConfig {
    fn default() -> Self {
        Self {
            margin_fps: 3.0,
            up_after: 2,
            down_after: 6,
        }
    }
}

impl TargetConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..=20.0).contains(&self.margin_fps) {
            return Err("target.margin_fps must be within 0..=20".into());
        }
        if self.up_after == 0 || self.down_after == 0 {
            return Err("target.up_after and target.down_after must be at least 1".into());
        }
        Ok(())
    }
}

/// Candidate rates usable on this display, ascending and de-duplicated.
pub fn candidates(configured: &[u32], refresh_hz: Option<f64>) -> Vec<u32> {
    let base: &[u32] = if configured.is_empty() {
        &DEFAULT_LADDER
    } else {
        configured
    };
    let limit = refresh_hz
        .filter(|r| r.is_finite() && *r >= 1.0)
        .map(|r| r.round() as u32);
    let mut v: Vec<u32> = base
        .iter()
        .copied()
        .filter(|c| *c >= 1 && limit.is_none_or(|l| *c <= l))
        .collect();
    if v.is_empty() {
        // Every configured rate exceeds the panel: the panel rate is the only honest target.
        if let Some(l) = limit {
            v.push(l);
        }
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// The candidate the observed rate is aiming for: the lowest one not clearly exceeded.
pub fn aim(observed_fps: f64, candidates: &[u32], margin_fps: f64) -> Option<u32> {
    let last = *candidates.last()?;
    Some(
        candidates
            .iter()
            .copied()
            .find(|c| observed_fps <= *c as f64 + margin_fps)
            .unwrap_or(last),
    )
}

#[derive(Debug, Clone)]
pub struct TargetSelector {
    cfg: TargetConfig,
    current: Option<u32>,
    pending: Option<(u32, u32)>,
}

impl TargetSelector {
    pub fn new(cfg: TargetConfig) -> Self {
        Self {
            cfg,
            current: None,
            pending: None,
        }
    }

    pub fn current(&self) -> Option<u32> {
        self.current
    }

    pub fn reset(&mut self) {
        self.current = None;
        self.pending = None;
    }

    /// Feed one evaluation. Returns the (possibly unchanged) target; `None` until a first
    /// target is known.
    pub fn update(&mut self, observed_fps: Option<f64>, candidates: &[u32]) -> Option<u32> {
        let Some(fps) = observed_fps.filter(|f| f.is_finite() && *f > 0.0) else {
            return self.current;
        };
        let Some(want) = aim(fps, candidates, self.cfg.margin_fps) else {
            return self.current;
        };
        match self.current {
            None => {
                // First target: start at the highest candidate the app could be aiming for,
                // so the analyzer does not judge a 60 FPS game against a 30 FPS deadline.
                self.current = Some(want);
                self.pending = None;
            }
            Some(cur) if want == cur => {
                self.pending = None;
            }
            Some(cur) => {
                let needed = if want > cur {
                    self.cfg.up_after
                } else {
                    self.cfg.down_after
                };
                let count = match self.pending {
                    Some((w, n)) if w == want => n + 1,
                    _ => 1,
                };
                if count >= needed {
                    self.current = Some(want);
                    self.pending = None;
                } else {
                    self.pending = Some((want, count));
                }
            }
        }
        // A target the display can no longer show (refresh rate dropped) is replaced at once.
        if let (Some(cur), Some(max)) = (self.current, candidates.last()) {
            if cur > *max {
                self.current = Some(*max);
            }
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_respect_refresh_rate() {
        assert_eq!(candidates(&[], Some(60.0)), vec![30, 45, 60]);
        assert_eq!(candidates(&[120, 60, 60], Some(90.0)), vec![60]);
        assert_eq!(candidates(&[120], Some(90.0)), vec![90]);
        assert_eq!(candidates(&[], None), DEFAULT_LADDER.to_vec());
    }

    #[test]
    fn aim_picks_lowest_not_exceeded() {
        let c = [30, 60, 90, 120];
        assert_eq!(aim(58.0, &c, 3.0), Some(60));
        assert_eq!(aim(62.5, &c, 3.0), Some(60));
        assert_eq!(aim(64.0, &c, 3.0), Some(90));
        assert_eq!(aim(200.0, &c, 3.0), Some(120));
        assert_eq!(aim(10.0, &[], 3.0), None);
    }

    #[test]
    fn switching_needs_agreement_and_down_is_slower() {
        let c = [30, 60, 120];
        let mut s = TargetSelector::new(TargetConfig::default());
        assert_eq!(s.update(Some(119.0), &c), Some(120));
        // One stutter does not lower the target.
        assert_eq!(s.update(Some(55.0), &c), Some(120));
        assert_eq!(s.update(Some(118.0), &c), Some(120));
        for _ in 0..5 {
            assert_eq!(s.update(Some(59.0), &c), Some(120));
        }
        assert_eq!(s.update(Some(59.0), &c), Some(60));
        // Raising needs two agreeing evaluations.
        assert_eq!(s.update(Some(119.0), &c), Some(60));
        assert_eq!(s.update(Some(119.0), &c), Some(120));
    }

    #[test]
    fn refresh_drop_clamps_immediately() {
        let mut s = TargetSelector::new(TargetConfig::default());
        s.update(Some(119.0), &[60, 120]);
        assert_eq!(s.update(Some(59.0), &[30, 60]), Some(60));
    }
}
