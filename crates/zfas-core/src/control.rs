//! Control abstraction: backends, time-bounded leases and conflict resolution.
//!
//! Nothing writes hardware directly from a decision. A decision becomes a *lease* (owner,
//! domain, level, expiry); the [`LeaseManager`] resolves all live leases per domain (highest
//! requested floor wins, bounded by the domain cap) and applies only changes. A lease that is
//! not renewed expires, so a stalled controller cannot leave a boost behind. Any backend error
//! makes the manager restore every domain and report the error.

use serde::{Deserialize, Serialize};

use crate::policy::MAX_LEVEL;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainKind {
    Cpu,
    Gpu,
}

/// One controllable domain (a cpufreq policy, a devfreq GPU).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainInfo {
    /// Stable identifier, e.g. `cpufreq/policy4` or `devfreq/3d00000.qcom,kgsl-3d0`.
    pub id: String,
    pub kind: DomainKind,
    /// CPUs in this domain (empty for GPU).
    pub cpus: Vec<u32>,
    /// `cpuinfo_max_freq` (kHz) or equivalent, used only for capacity weighting.
    pub capacity_khz: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlError {
    /// Reading or writing a control node failed.
    Io {
        domain: String,
        detail: String,
    },
    /// The node accepted the write but reads back a different value.
    Rejected {
        domain: String,
        wanted: u64,
        got: u64,
    },
    /// A value we set was changed by someone else (vendor perf HAL, another tool).
    Contention {
        domain: String,
        expected: u64,
        found: u64,
    },
    /// Restoring the original value failed: the device may be left modified.
    RestoreFailed {
        domain: String,
        detail: String,
    },
    UnknownDomain {
        index: usize,
    },
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::Io { domain, detail } => write!(f, "{domain}: I/O error: {detail}"),
            ControlError::Rejected {
                domain,
                wanted,
                got,
            } => {
                write!(f, "{domain}: wrote {wanted}, reads back {got}")
            }
            ControlError::Contention {
                domain,
                expected,
                found,
            } => {
                write!(
                    f,
                    "{domain}: expected {expected}, found {found} (changed by another agent)"
                )
            }
            ControlError::RestoreFailed { domain, detail } => {
                write!(f, "{domain}: restore failed: {detail}")
            }
            ControlError::UnknownDomain { index } => write!(f, "unknown control domain {index}"),
        }
    }
}

impl std::error::Error for ControlError {}

/// A hardware control backend. Level 0 means "original settings".
pub trait ControlBackend {
    fn domains(&self) -> &[DomainInfo];
    /// Apply `level` (0..=MAX_LEVEL) to one domain and verify it by reading back.
    fn set_level(&mut self, domain: usize, level: u8) -> Result<(), ControlError>;
    /// Check that every value this backend wrote is still in place.
    fn verify(&mut self) -> Result<(), ControlError>;
    /// Return every domain to the value it had before this backend touched it.
    fn restore_all(&mut self) -> Result<(), ControlError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub owner: String,
    pub domain: usize,
    pub level: u8,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    /// Levels in effect per domain after the sync.
    pub levels: Vec<u8>,
    /// Number of domains whose level changed.
    pub writes: u32,
}

#[derive(Debug, Clone)]
pub struct LeaseManager {
    leases: Vec<Lease>,
    applied: Vec<u8>,
    max_lease_ms: u64,
}

impl LeaseManager {
    /// `max_lease_ms` bounds every lease, whatever the owner asks for.
    pub fn new(domains: usize, max_lease_ms: u64) -> Self {
        Self {
            leases: Vec::new(),
            applied: vec![0; domains],
            max_lease_ms,
        }
    }

    pub fn leases(&self) -> &[Lease] {
        &self.leases
    }

    pub fn applied(&self) -> &[u8] {
        &self.applied
    }

    /// Grant or renew a lease. A level of 0 releases the owner's lease on that domain.
    pub fn grant(&mut self, owner: &str, domain: usize, level: u8, now_ms: u64, duration_ms: u64) {
        self.leases
            .retain(|l| !(l.owner == owner && l.domain == domain));
        if level == 0 || domain >= self.applied.len() {
            return;
        }
        self.leases.push(Lease {
            owner: owner.to_string(),
            domain,
            level: level.min(MAX_LEVEL),
            expires_ms: now_ms.saturating_add(duration_ms.min(self.max_lease_ms)),
        });
    }

    pub fn release_owner(&mut self, owner: &str) {
        self.leases.retain(|l| l.owner != owner);
    }

    pub fn release_all(&mut self) {
        self.leases.clear();
    }

    /// Resolve live leases: per domain the highest requested level, bounded by `caps`.
    pub fn resolve(&mut self, now_ms: u64, caps: &[u8]) -> Vec<u8> {
        self.leases.retain(|l| l.expires_ms > now_ms);
        let mut out = vec![0u8; self.applied.len()];
        for l in &self.leases {
            out[l.domain] = out[l.domain].max(l.level);
        }
        for (i, v) in out.iter_mut().enumerate() {
            *v = (*v).min(caps.get(i).copied().unwrap_or(0)).min(MAX_LEVEL);
        }
        out
    }

    /// Apply resolved levels. On any error every domain is restored and leases are dropped.
    pub fn sync(
        &mut self,
        now_ms: u64,
        caps: &[u8],
        backend: &mut dyn ControlBackend,
    ) -> Result<SyncReport, ControlError> {
        let want = self.resolve(now_ms, caps);
        let mut writes = 0;
        for (i, level) in want.iter().enumerate() {
            if self.applied[i] == *level {
                continue;
            }
            if let Err(e) = backend.set_level(i, *level) {
                self.fail_safe(backend);
                return Err(e);
            }
            self.applied[i] = *level;
            writes += 1;
        }
        Ok(SyncReport {
            levels: self.applied.clone(),
            writes,
        })
    }

    /// Verify the backend's values are still in place; restore everything on contention.
    pub fn verify(&mut self, backend: &mut dyn ControlBackend) -> Result<(), ControlError> {
        if self.applied.iter().all(|l| *l == 0) {
            return Ok(());
        }
        backend.verify().inspect_err(|_| self.fail_safe(backend))
    }

    /// Drop every lease and restore the original settings.
    pub fn restore(&mut self, backend: &mut dyn ControlBackend) -> Result<(), ControlError> {
        self.release_all();
        let r = backend.restore_all();
        self.applied.iter_mut().for_each(|l| *l = 0);
        r
    }

    fn fail_safe(&mut self, backend: &mut dyn ControlBackend) {
        // The original error is what the caller reports; a restore failure is surfaced by the
        // backend's own state (and by `zfasd restore` at the next start).
        let _ = self.restore(backend);
    }
}

/// In-memory backend for replay, observe-only runs and tests. It never touches the host.
#[derive(Debug, Clone)]
pub struct MemoryBackend {
    domains: Vec<DomainInfo>,
    pub levels: Vec<u8>,
    /// Test hook: fail `set_level` on this domain.
    pub fail_domain: Option<usize>,
    /// Test hook: report contention on the next verify.
    pub contended: bool,
    pub writes: u64,
}

impl MemoryBackend {
    pub fn new(domains: Vec<DomainInfo>) -> Self {
        let n = domains.len();
        Self {
            domains,
            levels: vec![0; n],
            fail_domain: None,
            contended: false,
            writes: 0,
        }
    }
}

impl ControlBackend for MemoryBackend {
    fn domains(&self) -> &[DomainInfo] {
        &self.domains
    }

    fn set_level(&mut self, domain: usize, level: u8) -> Result<(), ControlError> {
        if domain >= self.levels.len() {
            return Err(ControlError::UnknownDomain { index: domain });
        }
        if self.fail_domain == Some(domain) {
            return Err(ControlError::Io {
                domain: self.domains[domain].id.clone(),
                detail: "injected failure".into(),
            });
        }
        self.levels[domain] = level;
        self.writes += 1;
        Ok(())
    }

    fn verify(&mut self) -> Result<(), ControlError> {
        if self.contended {
            return Err(ControlError::Contention {
                domain: self
                    .domains
                    .first()
                    .map(|d| d.id.clone())
                    .unwrap_or_default(),
                expected: 0,
                found: 1,
            });
        }
        Ok(())
    }

    fn restore_all(&mut self) -> Result<(), ControlError> {
        self.levels.iter_mut().for_each(|l| *l = 0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(n: usize) -> MemoryBackend {
        MemoryBackend::new(
            (0..n)
                .map(|i| DomainInfo {
                    id: format!("cpufreq/policy{i}"),
                    kind: DomainKind::Cpu,
                    cpus: vec![i as u32],
                    capacity_khz: 2_000_000,
                })
                .collect(),
        )
    }

    #[test]
    fn highest_lease_wins_within_cap() {
        let mut b = backend(2);
        let mut m = LeaseManager::new(2, 10_000);
        m.grant("controller", 0, 4, 0, 1_000);
        m.grant("cli", 0, 7, 0, 1_000);
        m.grant("controller", 1, 9, 0, 1_000);
        let r = m.sync(0, &[10, 5], &mut b).unwrap();
        assert_eq!(r.levels, vec![7, 5]);
        assert_eq!(b.levels, vec![7, 5]);
    }

    #[test]
    fn leases_expire_and_are_bounded() {
        let mut b = backend(1);
        let mut m = LeaseManager::new(1, 2_000);
        m.grant("controller", 0, 5, 0, 1_000_000);
        assert_eq!(m.sync(1_999, &[10], &mut b).unwrap().levels, vec![5]);
        assert_eq!(m.sync(2_000, &[10], &mut b).unwrap().levels, vec![0]);
        assert_eq!(b.levels, vec![0]);
    }

    #[test]
    fn unchanged_levels_are_not_rewritten() {
        let mut b = backend(1);
        let mut m = LeaseManager::new(1, 10_000);
        m.grant("controller", 0, 3, 0, 5_000);
        m.sync(0, &[10], &mut b).unwrap();
        m.sync(100, &[10], &mut b).unwrap();
        assert_eq!(b.writes, 1);
    }

    #[test]
    fn write_failure_restores_everything() {
        let mut b = backend(2);
        let mut m = LeaseManager::new(2, 10_000);
        m.grant("controller", 0, 3, 0, 5_000);
        m.sync(0, &[10, 10], &mut b).unwrap();
        b.fail_domain = Some(1);
        m.grant("controller", 1, 3, 0, 5_000);
        assert!(m.sync(10, &[10, 10], &mut b).is_err());
        assert_eq!(b.levels, vec![0, 0]);
        assert!(m.leases().is_empty());
        assert_eq!(m.applied(), &[0, 0]);
    }

    #[test]
    fn contention_restores() {
        let mut b = backend(1);
        let mut m = LeaseManager::new(1, 10_000);
        m.grant("controller", 0, 3, 0, 5_000);
        m.sync(0, &[10], &mut b).unwrap();
        b.contended = true;
        assert!(matches!(
            m.verify(&mut b),
            Err(ControlError::Contention { .. })
        ));
        assert_eq!(b.levels, vec![0]);
    }

    #[test]
    fn zero_level_releases() {
        let mut b = backend(1);
        let mut m = LeaseManager::new(1, 10_000);
        m.grant("controller", 0, 3, 0, 5_000);
        m.sync(0, &[10], &mut b).unwrap();
        m.grant("controller", 0, 0, 10, 5_000);
        assert_eq!(m.sync(10, &[10], &mut b).unwrap().levels, vec![0]);
    }
}
