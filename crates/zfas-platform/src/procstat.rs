// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! Per-thread CPU busy fraction from `/proc/<pid>/task/<tid>/stat`.
//!
//! The `comm` field can contain spaces and parentheses, so fields are counted from the last
//! `)` rather than by naive whitespace splitting.

use std::collections::HashMap;

use crate::sysroot::SysRoot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadTimes {
    /// utime + stime in clock ticks.
    pub ticks: u64,
    /// CPU the thread last ran on.
    pub cpu: Option<u32>,
}

pub fn parse_stat(line: &str) -> Option<ThreadTimes> {
    let close = line.rfind(')')?;
    let rest: Vec<&str> = line[close + 1..].split_whitespace().collect();
    // rest[0] is field 3 (state); utime = field 14, stime = field 15, processor = field 39.
    let utime: u64 = rest.get(11)?.parse().ok()?;
    let stime: u64 = rest.get(12)?.parse().ok()?;
    let cpu = rest.get(36).and_then(|c| c.parse().ok());
    Some(ThreadTimes {
        ticks: utime + stime,
        cpu,
    })
}

pub fn clock_ticks_per_sec() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v > 0 {
        v as u64
    } else {
        100
    }
}

/// Tracks thread CPU time between samples.
#[derive(Debug, Default)]
pub struct BusySampler {
    pid: Option<u32>,
    last: HashMap<u32, u64>,
    last_ms: Option<u64>,
}

impl BusySampler {
    /// Highest single-thread busy fraction since the previous call, `None` on the first call
    /// for a process or if nothing could be read.
    pub fn sample(&mut self, root: &SysRoot, pid: u32, now_ms: u64, hz: u64) -> Option<f64> {
        if self.pid != Some(pid) {
            self.pid = Some(pid);
            self.last.clear();
            self.last_ms = None;
        }
        let mut now = HashMap::new();
        for tid in crate::foreground::tids_of(root, pid) {
            if let Some(t) = root
                .read_string(format!("/proc/{pid}/task/{tid}/stat"))
                .ok()
                .as_deref()
                .and_then(parse_stat)
            {
                now.insert(tid, t.ticks);
            }
        }
        if now.is_empty() {
            return None;
        }
        let result = self.last_ms.and_then(|prev_ms| {
            let dt_ms = now_ms.checked_sub(prev_ms).filter(|d| *d > 0)?;
            let budget = dt_ms as f64 * hz as f64 / 1000.0;
            now.iter()
                .filter_map(|(tid, t)| {
                    self.last
                        .get(tid)
                        .map(|p| t.saturating_sub(*p) as f64 / budget)
                })
                .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))))
                .map(|v| v.clamp(0.0, 1.0))
        });
        self.last = now;
        self.last_ms = Some(now_ms);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn stat(comm: &str, utime: u64, stime: u64) -> String {
        format!("1234 ({comm}) S 1 1 0 0 -1 4194624 100 0 0 0 {utime} {stime} 0 0 20 0 30 0 100 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 17 6 0 0 0 0 0")
    }

    #[test]
    fn comm_with_spaces_and_parens() {
        let t = parse_stat(&stat("Unity Main) (x", 50, 25)).unwrap();
        assert_eq!(t.ticks, 75);
        assert_eq!(t.cpu, Some(6));
        assert!(parse_stat("garbage").is_none());
    }

    #[test]
    fn busy_fraction_of_busiest_thread() {
        let d = TempDir::new("procstat");
        let root = SysRoot::at(d.path());
        d.file("proc/10/task/10/stat", &stat("main", 0, 0));
        d.file("proc/10/task/11/stat", &stat("render", 0, 0));
        let mut s = BusySampler::default();
        assert_eq!(s.sample(&root, 10, 0, 100), None);
        // 1 s later: main used 30 ticks, render 90 ticks of 100 available.
        d.file("proc/10/task/10/stat", &stat("main", 20, 10));
        d.file("proc/10/task/11/stat", &stat("render", 80, 10));
        let v = s.sample(&root, 10, 1_000, 100).unwrap();
        assert!((v - 0.9).abs() < 1e-9);
    }
}
