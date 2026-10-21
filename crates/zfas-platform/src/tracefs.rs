// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! Scheduler events from tracefs for the experimental CPCS analyzer.
//!
//! A private tracefs *instance* (`instances/zairenkai_fas`) is used so the global trace buffer,
//! Perfetto and other tools are not disturbed. The instance uses `trace_clock = mono`, the
//! same clock domain as SurfaceFlinger timestamps. `sched_waking` is preferred over
//! `sched_wakeup` because it fires in the waker's context; `sched_wakeup` may fire on the
//! target CPU, where the current task is not the waker.
//!
//! The instance is removed when the session is dropped.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use zfas_core::cpcs::{SchedEvent, SchedKind};
use zfas_core::model::{Clock, Timestamp};

use crate::sysroot::SysRoot;

pub const TRACEFS_CANDIDATES: [&str; 2] = ["/sys/kernel/tracing", "/sys/kernel/debug/tracing"];
pub const INSTANCE: &str = "zairenkai_fas";

pub fn tracefs_dir(root: &SysRoot) -> Option<&'static str> {
    TRACEFS_CANDIDATES
        .into_iter()
        .find(|d| root.exists(format!("{d}/instances")))
}

/// `5678.123456` (seconds) -> ns.
fn parse_secs(s: &str) -> Option<u64> {
    let (sec, frac) = s.split_once('.').unwrap_or((s, ""));
    let sec: u64 = sec.parse().ok()?;
    if frac.len() > 9 || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let frac_ns: u64 = if frac.is_empty() {
        0
    } else {
        frac.parse::<u64>().ok()? * 10u64.pow(9 - frac.len() as u32)
    };
    sec.checked_mul(1_000_000_000)?.checked_add(frac_ns)
}

fn field(body: &str, key: &str) -> Option<u32> {
    let pat = format!(" {key}=");
    let start = format!(" {body}").find(&pat)? + pat.len() - 1;
    body[start..].split_whitespace().next()?.parse().ok()
}

/// Parse one `trace_pipe` line. Unrelated events return `None`.
pub fn parse_line(line: &str) -> Option<SchedEvent> {
    // "<comm>-<pid> [(tgid)] [cpu] [flags] <secs>: <event>: <body>"
    let open = line.find(" [")?;
    let close = open + line[open..].find(']')?;
    let task = line[..open].trim();
    let task = match task.rfind(" (") {
        Some(i) if task.ends_with(')') => task[..i].trim_end(),
        _ => task,
    };
    let cpu: u32 = line[open + 2..close].trim().parse().ok()?;
    let current: u32 = task.rsplit_once('-')?.1.parse().ok()?;
    let after = &line[close + 1..];
    let (head, rest) = after.split_once(": ")?;
    let ts = parse_secs(head.split_whitespace().last()?)?;
    let (event, body) = rest.split_once(": ")?;
    let kind = match event.trim() {
        "sched_switch" => {
            let (prev, next) = body.split_once(" ==> ")?;
            SchedKind::Switch {
                prev_tid: field(prev, "prev_pid")?,
                next_tid: field(next, "next_pid")?,
            }
        }
        "sched_waking" | "sched_wakeup" => SchedKind::Wakeup {
            waker_tid: current,
            wakee_tid: field(body, "pid")?,
        },
        _ => return None,
    };
    Some(SchedEvent {
        ts: Timestamp::new(Clock::Monotonic, ts),
        cpu,
        kind,
    })
}

/// Remove a leftover instance from a crashed run (events off, tracing off, directory removed).
/// Returns `true` if an instance was found.
pub fn cleanup_instance(root: &SysRoot) -> bool {
    let mut found = false;
    for base in TRACEFS_CANDIDATES {
        let dir = format!("{base}/instances/{INSTANCE}");
        if !root.exists(&dir) {
            continue;
        }
        found = true;
        let _ = root.write_existing(format!("{dir}/tracing_on"), "0");
        for e in ["sched_switch", "sched_waking", "sched_wakeup"] {
            let _ = root.write_existing(format!("{dir}/events/sched/{e}/enable"), "0");
        }
        let _ = fs::remove_dir(root.path(&dir));
    }
    found
}

#[derive(Debug)]
pub struct TraceSession {
    root: SysRoot,
    dir: String,
    pipe: fs::File,
    partial: String,
    pub wake_event: &'static str,
    pub lines_malformed: u64,
}

impl TraceSession {
    /// Create and start the private instance. Requires root on a real device.
    pub fn start(root: &SysRoot) -> io::Result<Self> {
        let base = tracefs_dir(root)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "tracefs not mounted"))?;
        let dir = format!("{base}/instances/{INSTANCE}");
        if !root.exists(&dir) {
            fs::create_dir(root.path(&dir))?;
        }
        let setup = || -> io::Result<&'static str> {
            root.write_existing(format!("{dir}/tracing_on"), "0")?;
            root.write_existing(format!("{dir}/trace_clock"), "mono")?;
            let clock = root.read_string(format!("{dir}/trace_clock"))?;
            if !clock.contains("[mono]") && clock != "mono" {
                return Err(io::Error::other(format!("trace_clock not mono: {clock}")));
            }
            let _ = root.write_existing(format!("{dir}/buffer_size_kb"), "2048");
            root.write_existing(format!("{dir}/events/sched/sched_switch/enable"), "1")?;
            let wake = if root.exists(format!("{dir}/events/sched/sched_waking/enable")) {
                "sched_waking"
            } else {
                "sched_wakeup"
            };
            root.write_existing(format!("{dir}/events/sched/{wake}/enable"), "1")?;
            root.write_existing(format!("{dir}/tracing_on"), "1")?;
            Ok(wake)
        };
        let wake = match setup() {
            Ok(w) => w,
            Err(e) => {
                let _ = fs::remove_dir(root.path(&dir));
                return Err(e);
            }
        };
        let pipe = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(root.path(format!("{dir}/trace_pipe")))?;
        Ok(Self {
            root: root.clone(),
            dir,
            pipe,
            partial: String::new(),
            wake_event: wake,
            lines_malformed: 0,
        })
    }

    /// Drain whatever is buffered without blocking.
    pub fn drain(&mut self, max_bytes: usize) -> Vec<SchedEvent> {
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0;
        while total < max_bytes {
            match self.pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    self.partial.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::Interrupted =>
                {
                    break
                }
                Err(_) => break,
            }
        }
        let mut events = Vec::new();
        let keep_from = self.partial.rfind('\n').map(|i| i + 1).unwrap_or(0);
        for line in self.partial[..keep_from].lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            match parse_line(line) {
                Some(e) => events.push(e),
                None if line.contains("sched_switch:")
                    || line.contains(&format!("{}:", self.wake_event)) =>
                {
                    self.lines_malformed += 1;
                }
                None => {}
            }
        }
        self.partial.drain(..keep_from);
        events
    }

    pub fn instance_path(&self) -> PathBuf {
        self.root.path(&self.dir)
    }
}

impl Drop for TraceSession {
    fn drop(&mut self) {
        let _ = self
            .root
            .write_existing(format!("{}/tracing_on", self.dir), "0");
        for e in ["sched_switch", "sched_waking", "sched_wakeup"] {
            let _ = self
                .root
                .write_existing(format!("{}/events/sched/{e}/enable", self.dir), "0");
        }
        let _ = fs::remove_dir(self.root.path(&self.dir));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn parses_switch_and_waking_lines() {
        let l = "  Unity Main-1234  [003] d..2  5678.123456: sched_switch: prev_comm=Unity Main prev_pid=1234 prev_prio=110 prev_state=S ==> next_comm=swapper/3 next_pid=0 next_prio=120";
        let e = parse_line(l).unwrap();
        assert_eq!(e.cpu, 3);
        assert_eq!(e.ts.ns, 5_678_123_456_000);
        assert_eq!(
            e.kind,
            SchedKind::Switch {
                prev_tid: 1234,
                next_tid: 0
            }
        );
        let l = "  RenderThread-1240 ( 1200) [002] dNh3. 5678.123500: sched_waking: comm=UnityGfx pid=1241 prio=110 target_cpu=004";
        let e = parse_line(l).unwrap();
        assert_eq!(
            e.kind,
            SchedKind::Wakeup {
                waker_tid: 1240,
                wakee_tid: 1241
            }
        );
        assert_eq!(e.cpu, 2);
        assert!(parse_line("  foo-1 [000] .... 1.0: irq_handler_entry: irq=5").is_none());
        assert!(parse_line("garbage").is_none());
    }

    #[test]
    fn instance_lifecycle_on_fixture() {
        let d = TempDir::new("tracefs");
        let inst = "sys/kernel/tracing/instances/zairenkai_fas";
        // tracefs creates these when the instance directory is made; the fixture pre-creates them.
        for f in [
            "tracing_on",
            "buffer_size_kb",
            "events/sched/sched_switch/enable",
            "events/sched/sched_waking/enable",
        ] {
            d.file(&format!("{inst}/{f}"), "0");
        }
        d.file(&format!("{inst}/trace_clock"), "[mono]");
        d.file(
            &format!("{inst}/trace_pipe"),
            "  a-10 [001] d..2 1.000001: sched_switch: prev_comm=a prev_pid=10 prev_prio=1 prev_state=S ==> next_comm=b next_pid=11 next_prio=1\n  b-11 [001] d..2 1.000002: sched_waking: comm=c pid=12 prio=1 target_cpu=001\npartial",
        );
        let root = SysRoot::at(d.path());
        let mut s = TraceSession::start(&root).unwrap();
        assert_eq!(d.read(&format!("{inst}/tracing_on")), "1");
        assert_eq!(s.wake_event, "sched_waking");
        let ev = s.drain(1 << 20);
        assert_eq!(ev.len(), 2);
        assert_eq!(s.partial, "partial");
        drop(s);
        assert_eq!(d.read(&format!("{inst}/tracing_on")), "0");
        assert_eq!(
            d.read(&format!("{inst}/events/sched/sched_switch/enable")),
            "0"
        );
    }
}
