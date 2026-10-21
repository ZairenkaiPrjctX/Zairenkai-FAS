// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! Thermal inputs: Android thermal status via `dumpsys thermalservice`, and raw thermal
//! zones from sysfs (used only with configured thresholds).

use std::time::Duration;

use zfas_core::config::ZoneThreshold;
use zfas_core::thermal::{ThermalInput, ThermalStatus, ZoneReading};

use crate::exec::Runner;
use crate::sysroot::SysRoot;

/// `Thermal Status: N` from `dumpsys thermalservice` (Android 10+).
pub fn parse_thermal_status(text: &str) -> Option<ThermalStatus> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("Thermal Status:"))
        .and_then(|v| v.trim().parse::<i64>().ok())
        .and_then(ThermalStatus::from_code)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    pub dir: String,
    pub zone_type: String,
}

pub fn list_zones(root: &SysRoot) -> Vec<Zone> {
    let base = "/sys/class/thermal";
    root.list_dir(base)
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n.starts_with("thermal_zone"))
        .filter_map(|n| {
            let dir = format!("{base}/{n}");
            let zone_type = root.read_string(format!("{dir}/type")).ok()?;
            Some(Zone { dir, zone_type })
        })
        .collect()
}

pub struct ThermalReader {
    zones: Vec<(Zone, ZoneThreshold)>,
}

impl ThermalReader {
    /// Bind configured thresholds to zones present on this device. Returns unmatched names.
    pub fn new(root: &SysRoot, configured: &[ZoneThreshold]) -> (Self, Vec<String>) {
        let present = list_zones(root);
        let mut zones = Vec::new();
        let mut missing = Vec::new();
        for t in configured {
            match present.iter().find(|z| z.zone_type == t.zone_type) {
                Some(z) => zones.push((z.clone(), t.clone())),
                None => missing.push(t.zone_type.clone()),
            }
        }
        (Self { zones }, missing)
    }

    pub fn read(&self, root: &SysRoot, runner: &dyn Runner) -> ThermalInput {
        let status_out = runner.run(&["dumpsys", "thermalservice"], Duration::from_secs(2));
        let (status, mut read_error) = match &status_out {
            Ok(t) => (parse_thermal_status(t), false),
            Err(_) => (None, true),
        };
        let zones = self
            .zones
            .iter()
            .map(|(z, t)| {
                let mc = root.read_i64(format!("{}/temp", z.dir)).ok();
                read_error |= mc.is_none();
                ZoneReading {
                    name: z.zone_type.clone(),
                    millicelsius: mc,
                    derate_mc: t.derate_mc,
                    suspend_mc: t.suspend_mc,
                }
            })
            .collect();
        ThermalInput {
            status,
            zones,
            read_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::FakeRunner;
    use crate::testutil::TempDir;

    #[test]
    fn status_and_zones() {
        assert_eq!(
            parse_thermal_status("IsStatusOverride: false\nThermal Status: 2\n"),
            Some(ThermalStatus::Moderate)
        );
        assert_eq!(parse_thermal_status("Thermal Status: 9"), None);
        let d = TempDir::new("thermal");
        d.file("sys/class/thermal/thermal_zone0/type", "cpu-big");
        d.file("sys/class/thermal/thermal_zone0/temp", "71500");
        let root = SysRoot::at(d.path());
        let cfg = vec![
            ZoneThreshold {
                zone_type: "cpu-big".into(),
                derate_mc: Some(80_000),
                suspend_mc: Some(90_000),
            },
            ZoneThreshold {
                zone_type: "gpu".into(),
                derate_mc: None,
                suspend_mc: Some(1),
            },
        ];
        let (r, missing) = ThermalReader::new(&root, &cfg);
        assert_eq!(missing, vec!["gpu".to_string()]);
        let runner = FakeRunner::default().with("dumpsys thermalservice", "Thermal Status: 0\n");
        let i = r.read(&root, &runner);
        assert_eq!(i.status, Some(ThermalStatus::None));
        assert_eq!(i.zones[0].millicelsius, Some(71_500));
        assert!(!i.read_error);
        let i = r.read(&root, &FakeRunner::default());
        assert!(i.read_error);
        assert_eq!(i.status, None);
    }
}
