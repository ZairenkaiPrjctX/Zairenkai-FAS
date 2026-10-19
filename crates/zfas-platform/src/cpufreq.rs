//! cpufreq discovery and the CPU floor control backend.
//!
//! Only the standard Linux cpufreq sysfs ABI is used (`/sys/devices/system/cpu/cpufreq/
//! policyN`). The only node ever written is `scaling_min_freq`, and only with a value inside
//! `[cpuinfo_min_freq, scaling_max_freq]` read at that moment, so the configured or thermal
//! maximum is never raised and governors are never replaced.
//!
//! Before the first write the original value of every touched node is saved to a restore
//! file; `restore_all` (and `zfasd restore` after a crash) writes those values back.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zfas_core::control::{ControlBackend, ControlError, DomainInfo, DomainKind};
use zfas_core::policy::MAX_LEVEL;

use crate::fsutil::write_atomic;
use crate::sysroot::SysRoot;

pub const CPUFREQ_DIR: &str = "/sys/devices/system/cpu/cpufreq";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Directory name, e.g. `policy4`.
    pub name: String,
    pub cpus: Vec<u32>,
    pub cpuinfo_min_khz: u64,
    pub cpuinfo_max_khz: u64,
    pub scaling_min_khz: u64,
    pub scaling_max_khz: u64,
    /// Empty when the driver does not publish a frequency table.
    pub available_khz: Vec<u64>,
    pub governor: Option<String>,
}

impl Policy {
    pub fn dir(&self) -> String {
        format!("{CPUFREQ_DIR}/{}", self.name)
    }

    /// `scaling_max_freq` is below the hardware maximum: thermal or power management caps it.
    pub fn max_capped(&self) -> bool {
        self.scaling_max_khz < self.cpuinfo_max_khz
    }
}

fn parse_cpus(s: &str) -> Vec<u32> {
    // `affected_cpus` is space separated; `related_cpus` may use ranges ("0-3").
    let mut v = Vec::new();
    for tok in s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
    {
        if let Some((a, b)) = tok.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                v.extend(a..=b.min(a + 1024));
            }
        } else if let Ok(c) = tok.parse() {
            v.push(c);
        }
    }
    v
}

/// Read one policy; `None` if it is incomplete or inconsistent (then it is not controlled).
pub fn read_policy(root: &SysRoot, name: &str) -> Result<Policy, String> {
    let d = format!("{CPUFREQ_DIR}/{name}");
    let num = |f: &str| {
        root.read_u64(format!("{d}/{f}"))
            .map_err(|e| format!("{name}/{f}: {e}"))
    };
    let cpus = root
        .read_string(format!("{d}/related_cpus"))
        .or_else(|_| root.read_string(format!("{d}/affected_cpus")))
        .map(|s| parse_cpus(&s))
        .map_err(|e| format!("{name}/related_cpus: {e}"))?;
    let mut available: Vec<u64> = root
        .read_string(format!("{d}/scaling_available_frequencies"))
        .map(|s| {
            s.split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    available.sort_unstable();
    available.dedup();
    let p = Policy {
        name: name.to_string(),
        cpus,
        cpuinfo_min_khz: num("cpuinfo_min_freq")?,
        cpuinfo_max_khz: num("cpuinfo_max_freq")?,
        scaling_min_khz: num("scaling_min_freq")?,
        scaling_max_khz: num("scaling_max_freq")?,
        available_khz: available,
        governor: root.read_string(format!("{d}/scaling_governor")).ok(),
    };
    if p.cpus.is_empty() {
        return Err(format!("{name}: no CPUs"));
    }
    if p.cpuinfo_min_khz == 0
        || p.cpuinfo_min_khz > p.cpuinfo_max_khz
        || p.scaling_max_khz > p.cpuinfo_max_khz
    {
        return Err(format!("{name}: inconsistent frequency limits"));
    }
    Ok(p)
}

/// All usable policies, sorted by name; unusable ones are reported, not silently dropped.
pub fn discover(root: &SysRoot) -> (Vec<Policy>, Vec<String>) {
    let mut ok = Vec::new();
    let mut problems = Vec::new();
    let names = match root.list_dir(CPUFREQ_DIR) {
        Ok(n) => n,
        Err(e) => return (ok, vec![format!("{CPUFREQ_DIR}: {e}")]),
    };
    for n in names
        .into_iter()
        .filter(|n| n.starts_with("policy") && n[6..].chars().all(|c| c.is_ascii_digit()))
    {
        match read_policy(root, &n) {
            Ok(p) => ok.push(p),
            Err(e) => problems.push(e),
        }
    }
    ok.sort_by_key(|p| p.name[6..].parse::<u32>().unwrap_or(u32::MAX));
    (ok, problems)
}

/// Frequency floor for `level`: a fraction of `[cpuinfo_min, scaling_max]`, snapped down to
/// a published frequency. Never above the current `scaling_max_freq`.
pub fn floor_for_level(p: &Policy, level: u8, scaling_max_now: u64) -> u64 {
    let lo = p.cpuinfo_min_khz;
    let hi = scaling_max_now.min(p.cpuinfo_max_khz).max(lo);
    let level = level.min(MAX_LEVEL) as u64;
    let raw = lo + (hi - lo) * level / MAX_LEVEL as u64;
    let snapped = p
        .available_khz
        .iter()
        .copied()
        .filter(|f| *f <= raw && *f >= lo)
        .max()
        .unwrap_or(raw);
    snapped.clamp(lo, hi)
}

/// Original values to put back, persisted before the first write.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreFile {
    pub version: u32,
    /// Absolute node path -> original content.
    pub entries: BTreeMap<String, String>,
}

/// Only these nodes may ever be restored from a restore file (it is input from disk).
pub fn restorable(path: &str) -> bool {
    let Some(rest) = path.strip_prefix(&format!("{CPUFREQ_DIR}/policy")) else {
        return false;
    };
    let Some((num, file)) = rest.split_once('/') else {
        return false;
    };
    !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) && file == "scaling_min_freq"
}

/// Write back every entry of a restore file. Returns per-entry problems; on full success the
/// file is removed.
pub fn restore_from_file(root: &SysRoot, file: &Path) -> io::Result<Vec<String>> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let rf: RestoreFile =
        serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut problems = Vec::new();
    for (path, value) in &rf.entries {
        if !restorable(path) || value.parse::<u64>().is_err() {
            problems.push(format!("refusing to restore unexpected entry {path:?}"));
            continue;
        }
        if let Err(e) = root.write_existing(path, value) {
            problems.push(format!("{path}: {e}"));
        }
    }
    if problems.is_empty() {
        std::fs::remove_file(file)?;
    }
    Ok(problems)
}

#[derive(Debug)]
struct Slot {
    policy: Policy,
    original_min: Option<u64>,
    /// What we expect `scaling_min_freq` to read while we hold a floor.
    expected: Option<u64>,
}

/// CPU floor backend over cpufreq `scaling_min_freq`.
#[derive(Debug)]
pub struct CpufreqBackend {
    root: SysRoot,
    restore_path: PathBuf,
    domains: Vec<DomainInfo>,
    slots: Vec<Slot>,
}

impl CpufreqBackend {
    /// Control the given policies (already filtered by configuration).
    pub fn new(root: SysRoot, policies: Vec<Policy>, restore_path: PathBuf) -> Self {
        let domains = policies
            .iter()
            .map(|p| DomainInfo {
                id: format!("cpufreq/{}", p.name),
                kind: DomainKind::Cpu,
                cpus: p.cpus.clone(),
                capacity_khz: p.cpuinfo_max_khz,
            })
            .collect();
        let slots = policies
            .into_iter()
            .map(|policy| Slot {
                policy,
                original_min: None,
                expected: None,
            })
            .collect();
        Self {
            root,
            restore_path,
            domains,
            slots,
        }
    }

    pub fn policies(&self) -> impl Iterator<Item = &Policy> {
        self.slots.iter().map(|s| &s.policy)
    }

    fn node(slot: &Slot, file: &str) -> String {
        format!("{}/{file}", slot.policy.dir())
    }

    fn io_err(slot: &Slot, e: impl std::fmt::Display) -> ControlError {
        ControlError::Io {
            domain: format!("cpufreq/{}", slot.policy.name),
            detail: e.to_string(),
        }
    }

    fn persist_restore(&self) -> Result<(), ControlError> {
        let entries = self
            .slots
            .iter()
            .filter_map(|s| {
                s.original_min
                    .map(|v| (Self::node(s, "scaling_min_freq"), v.to_string()))
            })
            .collect();
        let rf = RestoreFile {
            version: 1,
            entries,
        };
        let json = serde_json::to_vec_pretty(&rf).expect("serializable");
        if let Some(dir) = self.restore_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        write_atomic(&self.restore_path, &json, 0o600).map_err(|e| ControlError::Io {
            domain: "restore-file".into(),
            detail: format!("{}: {e}", self.restore_path.display()),
        })
    }
}

impl ControlBackend for CpufreqBackend {
    fn domains(&self) -> &[DomainInfo] {
        &self.domains
    }

    fn set_level(&mut self, domain: usize, level: u8) -> Result<(), ControlError> {
        if domain >= self.slots.len() {
            return Err(ControlError::UnknownDomain { index: domain });
        }
        let min_node = Self::node(&self.slots[domain], "scaling_min_freq");
        let max_node = Self::node(&self.slots[domain], "scaling_max_freq");
        if self.slots[domain].original_min.is_none() {
            if level == 0 {
                return Ok(());
            }
            let cur = self
                .root
                .read_u64(&min_node)
                .map_err(|e| Self::io_err(&self.slots[domain], e))?;
            self.slots[domain].original_min = Some(cur);
            // Persist before touching the node, so a crash right after the write is recoverable.
            if let Err(e) = self.persist_restore() {
                self.slots[domain].original_min = None;
                return Err(e);
            }
        }
        let slot = &self.slots[domain];
        let original = slot.original_min.expect("set above");
        let max_now = self
            .root
            .read_u64(&max_node)
            .map_err(|e| Self::io_err(slot, e))?;
        let want = if level == 0 {
            original
        } else {
            floor_for_level(&slot.policy, level, max_now).max(original.min(max_now))
        };
        self.root
            .write_existing(&min_node, &want.to_string())
            .map_err(|e| Self::io_err(slot, e))?;
        let got = self
            .root
            .read_u64(&min_node)
            .map_err(|e| Self::io_err(slot, e))?;
        // The kernel clamps the effective minimum to the (possibly thermal-lowered) maximum.
        let acceptable = got == want || (got < want && got == max_now.min(want));
        if !acceptable {
            return Err(ControlError::Rejected {
                domain: format!("cpufreq/{}", slot.policy.name),
                wanted: want,
                got,
            });
        }
        self.slots[domain].expected = if level == 0 { None } else { Some(got) };
        Ok(())
    }

    fn verify(&mut self) -> Result<(), ControlError> {
        for slot in &self.slots {
            let Some(expected) = slot.expected else {
                continue;
            };
            let found = self
                .root
                .read_u64(Self::node(slot, "scaling_min_freq"))
                .map_err(|e| Self::io_err(slot, e))?;
            if found == expected {
                continue;
            }
            // Lowered because the maximum dropped below our floor: thermal capping, not a
            // competing writer. Accept; the thermal governor handles the cap.
            let max_now = self
                .root
                .read_u64(Self::node(slot, "scaling_max_freq"))
                .map_err(|e| Self::io_err(slot, e))?;
            if found < expected && found == max_now {
                continue;
            }
            return Err(ControlError::Contention {
                domain: format!("cpufreq/{}", slot.policy.name),
                expected,
                found,
            });
        }
        Ok(())
    }

    fn restore_all(&mut self) -> Result<(), ControlError> {
        let mut first_err = None;
        for slot in &mut self.slots {
            let Some(orig) = slot.original_min else {
                continue;
            };
            let node = format!("{}/scaling_min_freq", slot.policy.dir());
            match self.root.write_existing(&node, &orig.to_string()) {
                Ok(()) => {
                    slot.original_min = None;
                    slot.expected = None;
                }
                Err(e) => {
                    first_err.get_or_insert(ControlError::RestoreFailed {
                        domain: format!("cpufreq/{}", slot.policy.name),
                        detail: e.to_string(),
                    });
                }
            }
        }
        match first_err {
            Some(e) => {
                // Keep the restore file so `zfasd restore` can retry.
                let _ = self.persist_restore();
                Err(e)
            }
            None => {
                let _ = std::fs::remove_file(&self.restore_path);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testutil::TempDir;

    pub fn fixture(dir: &TempDir) {
        for (n, cpus, min, max, avail) in [
            (
                "policy0",
                "0 1 2 3",
                300_000u64,
                1_800_000u64,
                "300000 600000 900000 1200000 1500000 1800000",
            ),
            (
                "policy4",
                "4-6",
                500_000,
                2_400_000,
                "500000 1000000 1500000 2000000 2400000",
            ),
        ] {
            let d = format!("sys/devices/system/cpu/cpufreq/{n}");
            dir.file(&format!("{d}/related_cpus"), cpus);
            dir.file(&format!("{d}/cpuinfo_min_freq"), &min.to_string());
            dir.file(&format!("{d}/cpuinfo_max_freq"), &max.to_string());
            dir.file(&format!("{d}/scaling_min_freq"), &min.to_string());
            dir.file(&format!("{d}/scaling_max_freq"), &max.to_string());
            dir.file(&format!("{d}/scaling_available_frequencies"), avail);
            dir.file(&format!("{d}/scaling_governor"), "schedutil");
        }
        dir.file("sys/devices/system/cpu/cpufreq/policy9/related_cpus", "9");
    }

    fn setup() -> (TempDir, CpufreqBackend) {
        let dir = TempDir::new("cpufreq");
        fixture(&dir);
        let root = SysRoot::at(dir.path());
        let (policies, problems) = discover(&root);
        assert_eq!(policies.len(), 2);
        assert_eq!(
            problems.len(),
            1,
            "incomplete policy9 reported: {problems:?}"
        );
        let b = CpufreqBackend::new(root, policies, dir.path().join("data/restore.json"));
        (dir, b)
    }

    const P4_MIN: &str = "sys/devices/system/cpu/cpufreq/policy4/scaling_min_freq";
    const P4_MAX: &str = "sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq";

    #[test]
    fn discovery_parses_ranges_and_tables() {
        let (_d, b) = setup();
        let p: Vec<_> = b.policies().collect();
        assert_eq!(p[0].cpus, vec![0, 1, 2, 3]);
        assert_eq!(p[1].cpus, vec![4, 5, 6]);
        assert_eq!(p[1].available_khz.len(), 5);
        assert!(!p[1].max_capped());
    }

    #[test]
    fn floor_is_snapped_and_never_above_max() {
        let (_d, b) = setup();
        let p = b.policies().nth(1).unwrap().clone();
        assert_eq!(floor_for_level(&p, 0, 2_400_000), 500_000);
        assert_eq!(floor_for_level(&p, 5, 2_400_000), 1_000_000); // raw 1.45 GHz -> 1.0 GHz
        assert_eq!(floor_for_level(&p, 10, 2_400_000), 2_400_000);
        assert_eq!(floor_for_level(&p, 10, 1_500_000), 1_500_000);
        assert_eq!(floor_for_level(&p, 200, 2_400_000), 2_400_000);
    }

    #[test]
    fn write_readback_and_restore() {
        let (d, mut b) = setup();
        b.set_level(1, 10).unwrap();
        assert_eq!(d.read(P4_MIN), "2400000");
        let rf: RestoreFile = serde_json::from_str(&d.read("data/restore.json")).unwrap();
        assert_eq!(rf.entries.values().next().unwrap(), "500000");
        b.verify().unwrap();
        b.restore_all().unwrap();
        assert_eq!(d.read(P4_MIN), "500000");
        assert!(!d.path().join("data/restore.json").exists());
        // The max node was never written.
        assert_eq!(d.read(P4_MAX), "2400000");
    }

    #[test]
    fn foreign_change_is_contention_but_thermal_clamp_is_not() {
        let (d, mut b) = setup();
        b.set_level(1, 10).unwrap();
        // Thermal lowers the max; the kernel clamps the effective min to it.
        d.file(P4_MAX, "1500000");
        d.file(P4_MIN, "1500000");
        b.verify().unwrap();
        // Someone else writes the min.
        d.file(P4_MIN, "1000000");
        assert!(matches!(b.verify(), Err(ControlError::Contention { .. })));
    }

    #[test]
    fn crash_restore_from_file_only_touches_allowed_nodes() {
        let (d, mut b) = setup();
        b.set_level(1, 8).unwrap();
        drop(b); // simulated crash: no restore_all
        assert_ne!(d.read(P4_MIN), "500000");
        let root = SysRoot::at(d.path());
        let problems = restore_from_file(&root, &d.path().join("data/restore.json")).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(d.read(P4_MIN), "500000");

        d.file("data/evil.json", r#"{"version":1,"entries":{"/etc/passwd":"x","/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq":"1"}}"#);
        let problems = restore_from_file(&root, &d.path().join("data/evil.json")).unwrap();
        assert_eq!(problems.len(), 2);
        assert_eq!(d.read(P4_MAX), "2400000");
    }

    #[test]
    fn missing_node_is_an_error_not_a_new_file() {
        let (d, mut b) = setup();
        std::fs::remove_file(d.path().join(P4_MIN)).unwrap();
        assert!(b.set_level(1, 5).is_err());
        assert!(!d.path().join(P4_MIN).exists());
    }

    #[test]
    fn restorable_paths() {
        assert!(restorable(
            "/sys/devices/system/cpu/cpufreq/policy0/scaling_min_freq"
        ));
        assert!(!restorable(
            "/sys/devices/system/cpu/cpufreq/policy0/scaling_max_freq"
        ));
        assert!(!restorable(
            "/sys/devices/system/cpu/cpufreq/policy0/../../x/scaling_min_freq"
        ));
        assert!(!restorable(
            "/sys/devices/system/cpu/cpufreq/policy/scaling_min_freq"
        ));
    }
}
