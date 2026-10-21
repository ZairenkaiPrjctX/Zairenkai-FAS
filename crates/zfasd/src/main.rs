// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! `zfasd`: Zairenkai-FAS daemon and command-line tool.

mod bench;
mod daemon;
mod observer;
mod socket;
mod trace;
mod util;

use std::path::PathBuf;
use std::time::Duration;

use zfas_core::config::Config;
use zfas_core::control::ControlBackend;
use zfas_platform::exec::SystemRunner;
use zfas_platform::sysroot::SysRoot;
use zfas_platform::thermal::ThermalReader;

use crate::daemon::{load_config, restore_leftovers, Paths, RunOptions, DEFAULT_DIR};

const USAGE: &str = "\
zfasd - Zairenkai-FAS frame-aware scheduling daemon

USAGE:
  zfasd run [--dir DIR] [--observe-only]   run the daemon (needs root on a device)
  zfasd probe [--dir DIR]                  report device capabilities as JSON
  zfasd restore [--dir DIR]                put back values left by a crashed run
  zfasd ctl [--dir DIR] <status|enable|disable|reload|reset|profile NAME>
  zfasd record [--dir DIR] SECONDS OUT     record live observations (no control)
  zfasd replay [--config FILE] [--timeline] TRACE
  zfasd synth SCENARIO OUT                 write a synthetic trace (steady60|cpu_drop|thermal|pause)
  zfasd bench [SCENARIO] [ROUNDS]          engine overhead on synthetic input
  zfasd check-config FILE                  validate (and show migration notes for) a config
  zfasd default-config                     print the built-in defaults
  zfasd version

Profiles: power_saver, balanced, performance, fast_response, adaptive (experimental)
";

struct Args {
    dir: PathBuf,
    config: Option<PathBuf>,
    observe: bool,
    timeline: bool,
    rest: Vec<String>,
}

fn parse_args(raw: Vec<String>) -> Result<Args, String> {
    let mut a = Args {
        dir: PathBuf::from(DEFAULT_DIR),
        config: None,
        observe: false,
        timeline: false,
        rest: vec![],
    };
    let mut it = raw.into_iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--dir" => a.dir = it.next().ok_or("--dir needs a value")?.into(),
            "--config" => a.config = Some(it.next().ok_or("--config needs a value")?.into()),
            "--observe-only" => a.observe = true,
            "--timeline" => a.timeline = true,
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => a.rest.push(x),
        }
    }
    Ok(a)
}

fn print_json<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).expect("serializable"));
}

fn main() {
    let mut raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        eprint!("{USAGE}");
        std::process::exit(64);
    }
    let cmd = raw.remove(0);
    let args = match parse_args(raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("zfasd: {e}\n\n{USAGE}");
            std::process::exit(64);
        }
    };
    std::process::exit(dispatch(&cmd, args));
}

fn dispatch(cmd: &str, a: Args) -> i32 {
    let paths = Paths::new(&a.dir);
    match cmd {
        "run" => daemon::run(RunOptions {
            paths,
            root: SysRoot::real(),
            force_observe: a.observe,
            max_ticks: None,
            runner: Box::new(SystemRunner),
        }),
        "probe" => {
            print_json(&zfas_platform::probe::probe(
                &SysRoot::real(),
                &SystemRunner,
            ));
            0
        }
        "restore" => match restore_leftovers(&SysRoot::real(), &paths) {
            Ok(()) => {
                println!("restore: nothing left to restore");
                0
            }
            Err(e) => {
                eprintln!("restore: {e}");
                1
            }
        },
        "ctl" => {
            if a.rest.is_empty() {
                eprintln!("ctl: missing command");
                return 64;
            }
            let line = a.rest.join(" ");
            if let Err(e) = socket::parse_request(&line) {
                eprintln!("ctl: {e}");
                return 64;
            }
            match socket::request(&paths.socket(), &line) {
                Ok(r) => {
                    println!("{r}");
                    if r.contains("\"ok\":false") {
                        1
                    } else {
                        0
                    }
                }
                Err(e) => {
                    eprintln!(
                        "ctl: cannot reach daemon at {}: {e}",
                        paths.socket().display()
                    );
                    2
                }
            }
        }
        "record" => record(&a, &paths),
        "replay" => {
            let Some(path) = a.rest.first() else {
                eprintln!("replay: missing TRACE");
                return 64;
            };
            let cfg = match &a.config {
                Some(c) => match load_config(c) {
                    Ok((c, _)) => c,
                    Err(e) => {
                        eprintln!("replay: {e}");
                        return 2;
                    }
                },
                None => Config::default(),
            };
            match trace::read_trace(path.as_ref()) {
                Ok((h, steps)) => {
                    let (summary, timeline) = trace::replay(cfg, &h, steps, a.timeline);
                    for t in timeline {
                        println!("{t}");
                    }
                    print_json(&summary);
                    0
                }
                Err(e) => {
                    eprintln!("replay: {e}");
                    1
                }
            }
        }
        "synth" => {
            let (Some(s), Some(out)) = (a.rest.first(), a.rest.get(1)) else {
                eprintln!("synth: need SCENARIO OUT");
                return 64;
            };
            match trace::synthesize(s) {
                Ok((h, steps)) => match trace::write_trace(out.as_ref(), &h, &steps) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("synth: {e}");
                        1
                    }
                },
                Err(e) => {
                    eprintln!("synth: {e}");
                    64
                }
            }
        }
        "bench" => {
            let scenario = a.rest.first().map(String::as_str).unwrap_or("cpu_drop");
            let rounds = a.rest.get(1).and_then(|r| r.parse().ok()).unwrap_or(20);
            match bench::bench(scenario, rounds) {
                Ok(r) => {
                    print_json(&r);
                    0
                }
                Err(e) => {
                    eprintln!("bench: {e}");
                    64
                }
            }
        }
        "check-config" => {
            let Some(path) = a.rest.first() else {
                eprintln!("check-config: missing FILE");
                return 64;
            };
            match load_config(path.as_ref()) {
                Ok((_, notes)) => {
                    for n in notes {
                        println!("note: {n}");
                    }
                    println!("ok");
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    2
                }
            }
        }
        "default-config" => {
            print!("{}", Config::default().to_toml());
            0
        }
        "version" => {
            println!("zfasd {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            0
        }
        other => {
            eprintln!("zfasd: unknown command {other:?}\n\n{USAGE}");
            64
        }
    }
}

fn record(a: &Args, paths: &Paths) -> i32 {
    let (Some(secs), Some(out)) = (
        a.rest.first().and_then(|s| s.parse::<u64>().ok()),
        a.rest.get(1),
    ) else {
        eprintln!("record: need SECONDS OUT");
        return 64;
    };
    if secs == 0 || secs > 600 {
        eprintln!("record: SECONDS must be within 1..=600");
        return 64;
    }
    let cfg = load_config(&paths.config())
        .map(|c| c.0)
        .unwrap_or_default();
    let root = SysRoot::real();
    let runner = SystemRunner;
    let (policies, _) = zfas_platform::cpufreq::discover(&root);
    let domains: Vec<_> = zfas_platform::cpufreq::CpufreqBackend::new(
        root.clone(),
        policies.clone(),
        paths.restore(),
    )
    .domains()
    .to_vec();
    let (thermal, _) = ThermalReader::new(&root, &cfg.thermal.zones);
    let mut obs = observer::Observer::new(root, &runner, thermal, policies);
    obs.refresh_foreground();
    let Some(pkg) = obs.package.clone() else {
        eprintln!("record: no foreground app detected");
        return 1;
    };
    eprintln!("record: observing {pkg} for {secs} s (no control writes)");
    let mut steps = Vec::new();
    let end = util::monotonic_ms() + secs * 1_000;
    while util::monotonic_ms() < end {
        let t0 = util::monotonic_ms();
        steps.push(obs.observe());
        std::thread::sleep(Duration::from_millis(
            cfg.general
                .tick_ms
                .saturating_sub(util::monotonic_ms() - t0),
        ));
    }
    let header = trace::Header {
        format: trace::FORMAT.into(),
        version: 1,
        origin: "recorded".into(),
        package: Some(pkg),
        domains,
        tick_ms: cfg.general.tick_ms,
        note: format!(
            "recorded with zfasd {} (SurfaceFlinger latency, observe only)",
            env!("CARGO_PKG_VERSION")
        ),
    };
    match trace::write_trace(out.as_ref(), &header, &steps) {
        Ok(()) => {
            eprintln!("record: {} ticks written to {out}", steps.len());
            0
        }
        Err(e) => {
            eprintln!("record: {e}");
            1
        }
    }
}
