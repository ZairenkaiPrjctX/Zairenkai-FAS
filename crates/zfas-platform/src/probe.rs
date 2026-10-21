// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! Capability probe: what this device supports, with evidence, before anything is enabled.

use std::ffi::CString;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::exec::Runner;
use crate::sysroot::SysRoot;
use crate::{cpufreq, gpu, surfaceflinger, thermal, tracefs};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Supported,
    Unsupported,
    /// Present on the device but not implemented by this release.
    NotImplemented,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub status: Status,
    pub detail: String,
}

fn cap(name: &str, status: Status, detail: impl Into<String>) -> Capability {
    Capability {
        name: name.into(),
        status,
        detail: detail.into(),
    }
}

fn writable(root: &SysRoot, abs: &str) -> bool {
    let Ok(c) = CString::new(root.path(abs).to_string_lossy().as_bytes()) else {
        return false;
    };
    // SAFETY: valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

pub fn kernel_release() -> Option<String> {
    // SAFETY: uname fills a caller-provided struct.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return None;
    }
    let bytes: Vec<u8> = u
        .release
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Known vendor performance agents that may write the same nodes. Detection is by
/// well-known paths only; nothing is disabled.
const VENDOR_AGENTS: [(&str, &str); 4] = [
    ("/sys/kernel/fpsgo", "MediaTek FPSGO"),
    ("/proc/perfmgr", "MediaTek perfmgr"),
    ("/sys/module/migt", "Xiaomi MIGT"),
    (
        "/vendor/bin/hw/vendor.qti.hardware.perf2-hal-service",
        "Qualcomm perf HAL",
    ),
];

pub fn probe(root: &SysRoot, runner: &dyn Runner) -> Vec<Capability> {
    let mut out = Vec::new();
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    out.push(cap(
        "root",
        if euid == 0 {
            Status::Supported
        } else {
            Status::Unsupported
        },
        format!("euid {euid}"),
    ));
    out.push(cap(
        "android",
        if root.exists("/system/build.prop") {
            Status::Supported
        } else {
            Status::Unsupported
        },
        "/system/build.prop",
    ));
    out.push(cap(
        "kernel",
        Status::Unknown,
        kernel_release().unwrap_or_else(|| "unknown".into()),
    ));

    let (policies, problems) = cpufreq::discover(root);
    let w = policies
        .iter()
        .filter(|p| writable(root, &format!("{}/scaling_min_freq", p.dir())))
        .count();
    let detail = format!(
        "{} policies ({}), {} writable{}",
        policies.len(),
        policies
            .iter()
            .map(|p| format!("{}:{:?}", p.name, p.cpus))
            .collect::<Vec<_>>()
            .join(" "),
        w,
        if problems.is_empty() {
            String::new()
        } else {
            format!("; problems: {}", problems.join("; "))
        }
    );
    let st = if !policies.is_empty() && w == policies.len() {
        Status::Supported
    } else {
        Status::Unsupported
    };
    out.push(cap("control.cpufreq_min_floor", st, detail));
    let capped: Vec<_> = policies
        .iter()
        .filter(|p| p.max_capped())
        .map(|p| p.name.clone())
        .collect();
    out.push(cap(
        "evidence.cpu_max_capped",
        if policies.is_empty() {
            Status::Unsupported
        } else {
            Status::Supported
        },
        if capped.is_empty() {
            "no policy capped now".to_string()
        } else {
            format!("capped now: {}", capped.join(" "))
        },
    ));

    match runner.run(
        &["dumpsys", "SurfaceFlinger", "--list"],
        Duration::from_secs(2),
    ) {
        Ok(t) => {
            let n = surfaceflinger::parse_layer_list(&t).len();
            out.push(cap(
                "telemetry.surfaceflinger_latency",
                Status::Supported,
                format!("{n} layers listed"),
            ));
        }
        Err(e) => out.push(cap(
            "telemetry.surfaceflinger_latency",
            Status::Unsupported,
            e.to_string(),
        )),
    }
    match runner.run(&["dumpsys", "thermalservice"], Duration::from_secs(2)) {
        Ok(t) => match thermal::parse_thermal_status(&t) {
            Some(s) => out.push(cap("thermal.status", Status::Supported, format!("{s:?}"))),
            None => out.push(cap(
                "thermal.status",
                Status::Unsupported,
                "no 'Thermal Status' line",
            )),
        },
        Err(e) => out.push(cap("thermal.status", Status::Unsupported, e.to_string())),
    }
    let zones = thermal::list_zones(root);
    out.push(cap(
        "thermal.zones",
        if zones.is_empty() {
            Status::Unsupported
        } else {
            Status::Supported
        },
        format!(
            "{} zones (thresholds must be configured; none built in)",
            zones.len()
        ),
    ));
    out.push(match gpu::gpu_busy(root) {
        Some(v) => cap(
            "evidence.gpu_busy",
            Status::Supported,
            format!("kgsl {:.0}%", v * 100.0),
        ),
        None => cap(
            "evidence.gpu_busy",
            Status::Unsupported,
            "no kgsl gpu_busy_percentage; other vendors not supported",
        ),
    });
    out.push(cap(
        "control.gpu_floor",
        Status::NotImplemented,
        "GPU frequency control is not implemented in this release",
    ));

    let tdir = tracefs::tracefs_dir(root);
    out.push(cap(
        "cpcs.tracefs_sched",
        if tdir.is_some() {
            Status::Supported
        } else {
            Status::Unsupported
        },
        tdir.map(|d| format!("{d} (instances supported)"))
            .unwrap_or_else(|| "tracefs instances not found".into()),
    ));
    let btf = root.exists("/sys/kernel/btf/vmlinux");
    let uprobes = tdir.is_some_and(|d| root.exists(format!("{d}/uprobe_events")));
    out.push(cap(
        "telemetry.ebpf_uprobe",
        Status::NotImplemented,
        format!("not implemented in this release (BTF: {btf}, uprobe_events: {uprobes})"),
    ));
    out.push(cap(
        "telemetry.kernel_module",
        Status::NotImplemented,
        "no kernel module backend in this release",
    ));
    out.push(match root.read_string("/sys/fs/selinux/enforce") {
        Ok(v) => cap(
            "selinux",
            Status::Supported,
            if v == "1" { "enforcing" } else { "permissive" },
        ),
        Err(_) => cap("selinux", Status::Unknown, "enforce node unreadable"),
    });
    let agents: Vec<_> = VENDOR_AGENTS
        .iter()
        .filter(|(p, _)| root.exists(p))
        .map(|(_, n)| *n)
        .collect();
    out.push(cap(
        "conflicts.vendor_perf_agents",
        if agents.is_empty() {
            Status::Unknown
        } else {
            Status::Supported
        },
        if agents.is_empty() {
            "none of the known agents detected (others may exist)".to_string()
        } else {
            format!(
                "detected: {} (not disabled; contention is detected at runtime)",
                agents.join(", ")
            )
        },
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::FakeRunner;
    use crate::testutil::TempDir;

    #[test]
    fn probe_on_fixture() {
        let d = TempDir::new("probe");
        crate::cpufreq::tests::fixture(&d);
        d.file("sys/class/kgsl/kgsl-3d0/gpu_busy_percentage", "12 %");
        d.file("sys/kernel/fpsgo/x", "");
        let r = FakeRunner::default().with("dumpsys thermalservice", "Thermal Status: 1\n");
        let caps = probe(&SysRoot::at(d.path()), &r);
        let get = |n: &str| caps.iter().find(|c| c.name == n).unwrap().clone();
        assert_eq!(get("control.cpufreq_min_floor").status, Status::Supported);
        assert_eq!(get("thermal.status").status, Status::Supported);
        assert_eq!(
            get("telemetry.surfaceflinger_latency").status,
            Status::Unsupported
        );
        assert_eq!(get("evidence.gpu_busy").status, Status::Supported);
        assert_eq!(get("telemetry.ebpf_uprobe").status, Status::NotImplemented);
        assert!(get("conflicts.vendor_perf_agents").detail.contains("FPSGO"));
    }
}
