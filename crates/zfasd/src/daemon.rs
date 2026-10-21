// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! The daemon run loop.
//!
//! Order of safety measures:
//! 1. At start, write back any restore file left by a previous crash.
//! 2. Every write goes through the engine's leases (bounded, expiring, verified).
//! 3. A watchdog thread restores original values and aborts if the loop stalls.
//! 4. SIGTERM/SIGINT, a panic, or any exit path restores before the process ends.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use zfas_core::config::{self, Config, Mode};
use zfas_core::control::{ControlBackend, MemoryBackend};
use zfas_core::engine::{Engine, StepOutput};
use zfas_core::model::Clock;
use zfas_platform::cpufreq::{self, CpufreqBackend, Policy};
use zfas_platform::exec::Runner;
use zfas_platform::fsutil::write_atomic;
use zfas_platform::sysroot::SysRoot;
use zfas_platform::thermal::ThermalReader;
use zfas_platform::tracefs::TraceSession;

use crate::log;
use crate::observer::Observer;
use crate::socket::{Request, Server};
use crate::util::{install_signal_handlers, monotonic_ms, RELOAD, STOP};

pub const DEFAULT_DIR: &str = "/data/adb/zairenkai-fas";

#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
}

impl Paths {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
    pub fn config(&self) -> PathBuf {
        self.dir.join("config.toml")
    }
    pub fn restore(&self) -> PathBuf {
        self.dir.join("restore.json")
    }
    pub fn state(&self) -> PathBuf {
        self.dir.join("state.json")
    }
    pub fn socket(&self) -> PathBuf {
        self.dir.join("zfasd.sock")
    }
}

pub struct RunOptions {
    pub paths: Paths,
    pub root: SysRoot,
    pub force_observe: bool,
    /// Stop after this many ticks (tests and timed runs); `None` runs until signalled.
    pub max_ticks: Option<u64>,
    pub runner: Box<dyn Runner>,
}

pub fn load_config(path: &Path) -> Result<(Config, Vec<String>), String> {
    match std::fs::read_to_string(path) {
        Ok(t) => config::parse(&t).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((
            Config::default(),
            vec!["no config file; using defaults".into()],
        )),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn save_config(path: &Path, cfg: &Config) -> Result<(), String> {
    cfg.validate().map_err(|e| e.to_string())?;
    write_atomic(path, cfg.to_toml().as_bytes(), 0o600)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Crash recovery: put back values a previous run could not restore.
pub fn restore_leftovers(root: &SysRoot, paths: &Paths) -> Result<(), String> {
    if zfas_platform::tracefs::cleanup_instance(root) {
        log!("INFO", "removed leftover tracefs instance");
    }
    match cpufreq::restore_from_file(root, &paths.restore()) {
        Ok(p) if p.is_empty() => Ok(()),
        Ok(p) => Err(p.join("; ")),
        Err(e) => Err(e.to_string()),
    }
}

fn controlled_policies(root: &SysRoot, cfg: &Config) -> (Vec<Policy>, Vec<String>) {
    let (all, problems) = cpufreq::discover(root);
    let p = all
        .into_iter()
        .filter(|p| !cfg.control.exclude_policies.contains(&p.name))
        .collect();
    (p, problems)
}

#[derive(Serialize)]
struct DomainState<'a> {
    id: &'a str,
    level: u8,
}

#[derive(Serialize)]
struct Snapshot<'a> {
    version: u32,
    pid: u32,
    running: bool,
    updated_mono_ms: u64,
    mode: Mode,
    observe_only: bool,
    package: Option<&'a str>,
    layer: Option<&'a str>,
    /// `general.enabled` (master switch).
    enabled: bool,
    /// `general.profile` (default profile; `profile` is the one applied to the current app).
    default_profile: &'a str,
    app_enabled: bool,
    profile: &'a str,
    telemetry_error: Option<&'a str>,
    step: Option<&'a StepOutput>,
    domains: Vec<DomainState<'a>>,
    cpcs_enabled: bool,
    diag: &'a zfas_core::diag::SessionDiag,
    loop_ms_avg: f64,
    loop_ms_max: u64,
    errors: &'a [String],
}

struct Runtime<'a> {
    cfg: Config,
    engine: Engine,
    backend: Box<dyn ControlBackend>,
    observe_only: bool,
    observer: Observer<'a>,
    last: Option<StepOutput>,
    errors: Vec<String>,
    loop_total_ms: u64,
    loop_max_ms: u64,
    loops: u64,
}

impl Runtime<'_> {
    fn note_error(&mut self, e: impl Into<String>) {
        let e = e.into();
        log!("WARN", "{e}");
        if self.errors.len() == 8 {
            self.errors.remove(0);
        }
        self.errors.push(e);
    }

    fn snapshot(&self, running: bool) -> Vec<u8> {
        let app = self.engine.app();
        let levels = self
            .last
            .as_ref()
            .map(|s| s.levels.clone())
            .unwrap_or_default();
        let s = Snapshot {
            version: 1,
            pid: std::process::id(),
            running,
            updated_mono_ms: monotonic_ms(),
            mode: self.cfg.general.mode,
            observe_only: self.observe_only,
            package: self.observer.package.as_deref(),
            layer: self.observer.layer(),
            enabled: self.cfg.general.enabled,
            default_profile: self.cfg.general.profile.as_str(),
            app_enabled: app.enabled,
            profile: app.profile.as_str(),
            telemetry_error: self.observer.last_telemetry_error.as_deref(),
            step: self.last.as_ref(),
            domains: self
                .engine
                .domains()
                .iter()
                .enumerate()
                .map(|(i, d)| DomainState {
                    id: &d.id,
                    level: levels.get(i).copied().unwrap_or(0),
                })
                .collect(),
            cpcs_enabled: self.engine.cpcs_enabled(),
            diag: &self.engine.diag,
            loop_ms_avg: if self.loops == 0 {
                0.0
            } else {
                self.loop_total_ms as f64 / self.loops as f64
            },
            loop_ms_max: self.loop_max_ms,
            errors: &self.errors,
        };
        serde_json::to_vec_pretty(&s).expect("serializable")
    }

    fn handle(&mut self, req: Result<Request, String>, paths: &Paths) -> String {
        let ok = |msg: &str| serde_json::json!({"ok": true, "message": msg}).to_string();
        let req = match req {
            Ok(r) => r,
            Err(e) => return serde_json::json!({"ok": false, "error": e}).to_string(),
        };
        let mut new_cfg = self.cfg.clone();
        match req {
            Request::Status => {
                return String::from_utf8(self.snapshot(true))
                    .unwrap_or_default()
                    .replace('\n', " ")
            }
            Request::Reset => {
                let app = self.engine.app().clone();
                if let Err(e) = self.engine.set_app(app, self.backend.as_mut()) {
                    self.note_error(format!("reset: {e}"));
                }
                return ok("boosts released, history cleared");
            }
            Request::Reload => match load_config(&paths.config()) {
                Ok((c, _)) => new_cfg = c,
                Err(e) => return serde_json::json!({"ok": false, "error": e}).to_string(),
            },
            Request::Enable => new_cfg.general.enabled = true,
            Request::Disable => new_cfg.general.enabled = false,
            Request::Profile(p) => new_cfg.general.profile = p,
        }
        if !matches!(req, Request::Reload) {
            if let Err(e) = save_config(&paths.config(), &new_cfg) {
                return serde_json::json!({"ok": false, "error": e}).to_string();
            }
        }
        self.apply_config(new_cfg);
        ok("applied")
    }

    fn apply_config(&mut self, cfg: Config) {
        let app = cfg.for_app(self.observer.package.as_deref());
        if let Err(e) = self
            .engine
            .set_config(cfg.clone(), app, self.backend.as_mut())
        {
            self.note_error(format!("apply config: {e}"));
        }
        self.cfg = cfg;
    }
}

fn spawn_watchdog(heartbeat: Arc<AtomicU64>, root: SysRoot, paths: Paths, stall_ms: u64) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(1_000));
        let last = heartbeat.load(Ordering::SeqCst);
        if last != 0 && monotonic_ms().saturating_sub(last) > stall_ms {
            log!(
                "ERROR",
                "main loop stalled for more than {stall_ms} ms: restoring and aborting"
            );
            if let Err(e) = restore_leftovers(&root, &paths) {
                log!("ERROR", "watchdog restore: {e}");
            }
            std::process::abort();
        }
    });
}

pub fn run(opts: RunOptions) -> i32 {
    install_signal_handlers();
    let paths = opts.paths.clone();
    if let Err(e) = std::fs::create_dir_all(&paths.dir) {
        log!("ERROR", "{}: {e}", paths.dir.display());
        return 1;
    }
    if let Err(e) = restore_leftovers(&opts.root, &paths) {
        log!(
            "ERROR",
            "restoring values from a previous run failed: {e}; refusing to control"
        );
        return 3;
    }
    let (cfg, notes) = match load_config(&paths.config()) {
        Ok(c) => c,
        Err(e) => {
            log!("ERROR", "{e}");
            return 2;
        }
    };
    for n in notes {
        log!("INFO", "config: {n}");
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_loop(&opts, cfg)));
    // Whatever happened, leave nothing behind.
    if let Err(e) = restore_leftovers(&opts.root, &paths) {
        log!("ERROR", "final restore: {e}");
    }
    match result {
        Ok(code) => code,
        Err(_) => {
            log!("ERROR", "panic in main loop; original values restored");
            70
        }
    }
}

fn run_loop(opts: &RunOptions, cfg: Config) -> i32 {
    let paths = &opts.paths;
    let observe_only = opts.force_observe || cfg.general.mode == Mode::Observe || !cfg.control.cpu;
    let (policies, problems) = controlled_policies(&opts.root, &cfg);
    for p in &problems {
        log!("WARN", "cpufreq: {p}");
    }
    let backend: Box<dyn ControlBackend> = if observe_only {
        let b = CpufreqBackend::new(opts.root.clone(), policies.clone(), paths.restore());
        Box::new(MemoryBackend::new(b.domains().to_vec()))
    } else {
        Box::new(CpufreqBackend::new(
            opts.root.clone(),
            policies.clone(),
            paths.restore(),
        ))
    };
    log!(
        "INFO",
        "zfasd {} starting: {} cpufreq policies, mode {}",
        env!("CARGO_PKG_VERSION"),
        policies.len(),
        if observe_only { "observe" } else { "control" }
    );
    let (thermal, missing) = ThermalReader::new(&opts.root, &cfg.thermal.zones);
    for m in missing {
        log!("WARN", "configured thermal zone {m:?} not present");
    }
    let mut observer = Observer::new(opts.root.clone(), opts.runner.as_ref(), thermal, policies);
    if cfg.cpcs.enabled {
        match TraceSession::start(&opts.root) {
            Ok(t) => {
                log!(
                    "INFO",
                    "CPCS (experimental): tracefs instance started using {}",
                    t.wake_event
                );
                observer.trace = Some(t);
            }
            Err(e) => log!("WARN", "CPCS disabled: tracefs unavailable: {e}"),
        }
    }
    let mut cfg_run = cfg.clone();
    if observer.trace.is_none() {
        cfg_run.cpcs.enabled = false;
    }
    let app = cfg_run.for_app(None);
    let domains = backend.domains().to_vec();
    let mut rt = Runtime {
        engine: Engine::new(cfg_run.clone(), app, domains, Clock::Monotonic),
        cfg: cfg_run,
        backend,
        observe_only,
        observer,
        last: None,
        errors: vec![],
        loop_total_ms: 0,
        loop_max_ms: 0,
        loops: 0,
    };
    let server = match Server::bind(&paths.socket()) {
        Ok(s) => Some(s),
        Err(e) => {
            rt.note_error(format!("control socket: {e}"));
            None
        }
    };
    let heartbeat = Arc::new(AtomicU64::new(monotonic_ms()));
    spawn_watchdog(heartbeat.clone(), opts.root.clone(), paths.clone(), 15_000);

    let mut tick: u64 = 0;
    let mut last_state_write = 0;
    while !STOP.load(Ordering::SeqCst) {
        let started = monotonic_ms();
        heartbeat.store(started, Ordering::SeqCst);
        if RELOAD.swap(false, Ordering::SeqCst) {
            match load_config(&paths.config()) {
                Ok((c, _)) => {
                    log!("INFO", "configuration reloaded");
                    rt.apply_config(c);
                }
                Err(e) => rt.note_error(format!("reload rejected, keeping previous config: {e}")),
            }
        }
        if let Some(s) = &server {
            s.poll(|r| rt.handle(r, paths));
        }
        if tick % 4 == 0 && rt.observer.refresh_foreground() {
            let app = rt.cfg.for_app(rt.observer.package.as_deref());
            log!(
                "INFO",
                "foreground: {} (enabled: {}, profile: {})",
                rt.observer.package.as_deref().unwrap_or("-"),
                app.enabled,
                app.profile.as_str()
            );
            if let Err(e) = rt.engine.set_app(app, rt.backend.as_mut()) {
                rt.note_error(format!("release on app change: {e}"));
            }
            let tids = rt.observer.target_tids();
            rt.engine.set_cpcs_targets(&tids);
        } else if tick % 8 == 0 && rt.engine.cpcs_enabled() {
            let tids = rt.observer.target_tids();
            rt.engine.set_cpcs_targets(&tids);
        }
        let input = rt.observer.observe();
        let out = rt.engine.step(input, rt.backend.as_mut());
        if let Some(e) = &out.control_error {
            rt.note_error(format!("control fault, boosts released: {e}"));
        }
        if rt
            .last
            .as_ref()
            .is_none_or(|l| l.decision.state != out.decision.state)
        {
            log!(
                "INFO",
                "state {:?}: {}",
                out.decision.state,
                out.decision.reason
            );
        }
        rt.last = Some(out);
        let elapsed = monotonic_ms().saturating_sub(started);
        rt.loops += 1;
        rt.loop_total_ms += elapsed;
        rt.loop_max_ms = rt.loop_max_ms.max(elapsed);
        if started.saturating_sub(last_state_write) >= 2_000 {
            last_state_write = started;
            if let Err(e) = write_atomic(&paths.state(), &rt.snapshot(true), 0o644) {
                log!("WARN", "state file: {e}");
            }
        }
        tick += 1;
        if opts.max_ticks.is_some_and(|m| tick >= m) {
            break;
        }
        let period = rt.cfg.general.tick_ms;
        let sleep = period.saturating_sub(monotonic_ms().saturating_sub(started));
        // Sleep in small steps so signals are handled promptly.
        let until = monotonic_ms() + sleep;
        while monotonic_ms() < until && !STOP.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(sleep.min(50)));
        }
    }
    let code = match rt.engine.shutdown(rt.backend.as_mut()) {
        Ok(()) => 0,
        Err(e) => {
            log!("ERROR", "shutdown restore: {e}");
            4
        }
    };
    heartbeat.store(0, Ordering::SeqCst);
    let _ = write_atomic(&paths.state(), &rt.snapshot(false), 0o644);
    log!("INFO", "stopped; original settings restored");
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use zfas_platform::exec::FakeRunner;
    use zfas_platform::testutil::TempDir;

    const P4_MIN: &str = "sys/devices/system/cpu/cpufreq/policy4/scaling_min_freq";

    fn fixture(d: &TempDir) {
        for (n, cpus, min, max) in [
            ("policy0", "0-3", 300_000u64, 1_800_000u64),
            ("policy4", "4-7", 500_000, 2_400_000),
        ] {
            let p = format!("sys/devices/system/cpu/cpufreq/{n}");
            d.file(&format!("{p}/related_cpus"), cpus);
            d.file(&format!("{p}/cpuinfo_min_freq"), &min.to_string());
            d.file(&format!("{p}/cpuinfo_max_freq"), &max.to_string());
            d.file(&format!("{p}/scaling_min_freq"), &min.to_string());
            d.file(&format!("{p}/scaling_max_freq"), &max.to_string());
        }
    }

    #[test]
    fn lifecycle_restores_leftovers_and_writes_state() {
        let d = TempDir::new("daemon");
        fixture(&d);
        // A previous run crashed while holding a floor on policy4.
        d.file(P4_MIN, "2400000");
        d.file(
            "data/restore.json",
            r#"{"version":1,"entries":{"/sys/devices/system/cpu/cpufreq/policy4/scaling_min_freq":"500000"}}"#,
        );
        let runner = FakeRunner::default()
            .with(
                "dumpsys activity activities",
                "  topResumedActivity=ActivityRecord{1 u0 com.example.game/.Main t1}\n",
            )
            .with("dumpsys thermalservice", "Thermal Status: 0\n");
        let code = run_loop_for_test(&d, Box::new(runner), 3);
        assert_eq!(code, 0);
        assert_eq!(d.read(P4_MIN), "500000", "leftover floor restored at start");
        assert!(!d.path().join("data/restore.json").exists());
        let state: serde_json::Value = serde_json::from_str(&d.read("data/state.json")).unwrap();
        assert_eq!(state["running"], false);
        assert_eq!(state["package"], "com.example.game");
        assert_eq!(state["enabled"], true);
        assert_eq!(state["default_profile"], "balanced");
        // dumpsys SurfaceFlinger is not available: no telemetry, so no boost was applied.
        assert_eq!(state["step"]["decision"]["state"], "telemetry_unavailable");
        assert!(state["telemetry_error"]
            .as_str()
            .unwrap()
            .contains("SurfaceFlinger"));
        assert!(
            !d.path().join("data/zfasd.sock").exists(),
            "socket removed on exit"
        );
    }

    #[test]
    fn invalid_config_refuses_to_start() {
        let d = TempDir::new("daemon-cfg");
        fixture(&d);
        d.file("data/config.toml", "[general]\ntick_ms = 1\n");
        assert_eq!(run_loop_for_test(&d, Box::new(FakeRunner::default()), 1), 2);
    }

    /// Plays a game rendering at 48 FPS (target 60) through fake `dumpsys` output with live
    /// monotonic timestamps, like SurfaceFlinger's 128-frame history.
    struct ScriptedRunner;

    impl Runner for ScriptedRunner {
        fn run(
            &self,
            argv: &[&str],
            _t: Duration,
        ) -> Result<String, zfas_platform::exec::ExecError> {
            let layer = "SurfaceView[com.example.game/com.example.game.Main](BLAST)#7";
            Ok(match argv.join(" ").as_str() {
                "dumpsys activity activities" => {
                    "topResumedActivity=ActivityRecord{1 u0 com.example.game/.Main t1}\n".into()
                }
                "dumpsys thermalservice" => "Thermal Status: 0\n".into(),
                "dumpsys SurfaceFlinger --list" => format!("{layer}\n"),
                a if a == format!("dumpsys SurfaceFlinger --latency {layer}") => {
                    let period = 1_000_000_000 / 48;
                    let now = crate::util::monotonic_ns();
                    let last = now - now % period;
                    let mut out = String::from("8333333\n");
                    for i in (0..127u64).rev() {
                        let t = last - i * period;
                        out.push_str(&format!("{t}\t{t}\t{}\n", t - 1_000_000));
                    }
                    out
                }
                other => {
                    return Err(zfas_platform::exec::ExecError::Failed {
                        status: Some(1),
                        stderr: other.into(),
                    })
                }
            })
        }
    }

    #[test]
    fn end_to_end_boost_is_applied_bounded_and_restored() {
        let d = TempDir::new("daemon-e2e");
        fixture(&d);
        d.file(
            "data/config.toml",
            "[general]\ntick_ms = 200\n[apps.\"com.example.game\"]\ntarget_fps = [60]\n",
        );
        let code = run_loop_for_test(&d, Box::new(ScriptedRunner), 40);
        assert_eq!(code, 0);
        let state: serde_json::Value = serde_json::from_str(&d.read("data/state.json")).unwrap();
        let max = state["diag"]["max_cpu_level"].as_u64().unwrap();
        // No CPU-busy evidence in the fixture: bottleneck unknown, so the conservative cap applies.
        assert!(
            max > 0 && max <= 3,
            "boosted within the unknown-bottleneck cap: {state}"
        );
        assert_eq!(state["step"]["target_fps"], 60);
        assert_eq!(d.read(P4_MIN), "500000", "floor restored at shutdown");
        assert_eq!(
            d.read("sys/devices/system/cpu/cpufreq/policy0/scaling_min_freq"),
            "300000"
        );
        assert_eq!(
            d.read("sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq"),
            "2400000"
        );
        assert!(!d.path().join("data/restore.json").exists());
    }

    fn run_loop_for_test(d: &TempDir, runner: Box<dyn Runner>, ticks: u64) -> i32 {
        run(RunOptions {
            paths: Paths::new(d.path().join("data")),
            root: SysRoot::at(d.path()),
            force_observe: false,
            max_ticks: Some(ticks),
            runner,
        })
    }
}
