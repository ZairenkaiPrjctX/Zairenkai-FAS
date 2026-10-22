# Zairenkai-FAS architecture

Zairenkai-FAS is a root daemon (`zfasd`) packaged as a Magisk/KernelSU module. It watches the
foreground app's frame delivery and, when frames miss their deadline because the CPU is the
bottleneck, raises **CPU frequency floors** (`scaling_min_freq`). The floors are bounded and
time-limited, and the daemon restores the original values when it stops or fails.

## Crates

| Crate | Responsibility | I/O |
|---|---|---|
| `crates/zfas-core` | Telemetry model, frame analyzer, target-FPS selection, profiles, thermal governor, controller state machine, control leases, CPCS analyzer, config, diagnostics, and the `Engine` that ties them together | none (pure, deterministic) |
| `crates/zfas-platform` | Adapters for SurfaceFlinger, `dumpsys activity`, cpufreq sysfs, thermal service and zones, `/proc` stats, kgsl, tracefs; capability probe; atomic writes; timed command runner | all through `SysRoot` (test root) and `Runner` (fake commands) |
| `crates/zfasd` | Daemon loop, watchdog, control socket, state file, CLI (probe/restore/ctl/record/replay/synth/bench) | real device |
| `scripts/build-android.sh` | Android arm64 component (`dist/zairenkai-fas/`) for the `zairenkai` module in Zairenkai-Modules | build |

## One tick (default 500 ms)

```
dumpsys activity ──► foreground package ──► pid, tids, per-app settings
dumpsys SurfaceFlinger --latency <layer> ──► FrameEvent[] (CLOCK_MONOTONIC)
/proc/<pid>/task/*/stat ──► max thread busy      ┐
kgsl gpu_busy_percentage ──► gpu busy             ├─► LoadEvidence
cpufreq scaling_max < cpuinfo_max ──► capped      ┘
dumpsys thermalservice + configured zones ──► ThermalInput
tracefs sched_switch/sched_waking (CPCS only) ──► SchedEvent[]
                    │
                    ▼  Engine::step (zfas-core/src/engine.rs)
FrameAnalyzer ─► WindowStats ─► TargetSelector ─► target FPS
ThermalGovernor ─► ThermalLimit (cap, suspended)
Controller::tick ─► Decision {state, cpu_level 0..=10, bottleneck, reason}
CPCS weights (fallback: uniform) ─► per-domain level
LeaseManager ─► (expiring leases, cap) ─► ControlBackend::set_level / verify
                    │
                    ▼
CpufreqBackend: scaling_min_freq = snap(cpuinfo_min + level/10 · (scaling_max − cpuinfo_min))
```

The daemon and `zfasd replay` both call `Engine::step`. Replay therefore exercises the same
code as the device, with `MemoryBackend` in place of cpufreq.

## Key design decisions

- **Telemetry source.** `dumpsys SurfaceFlinger --latency` needs no kernel features, works
  for every app, and reports the display-side present time, which is what users perceive.
  Each tick costs a process spawn (measured on device by `loop_ms_avg` in the state file).
  eBPF and kernel-module frame sources are capability-probed but not implemented
  (see `CAPABILITIES.md`).
- **Clock domains.** Every timestamp carries its clock (`model::Clock`). Durations between
  clocks are refused rather than guessed. Tracefs is set to `trace_clock = mono`, the same
  domain as SurfaceFlinger.
- **Levels, not frequencies.** Profiles use levels 0..=10, a fraction of each policy's range
  measured at write time. No profile names a frequency, and no device path is hard-coded in
  the generic logic.
- **Floors only.** Only `scaling_min_freq` is written, with values between `cpuinfo_min_freq`
  and the current `scaling_max_freq`. The thermal or vendor maximum is never raised, and
  governors are never changed.
- **Leases.** A decision becomes a lease that expires after three ticks. If the loop stops
  renewing it, the boost is released.
- **Verification.** Each write is read back. Every `verify_every_ticks` ticks the expected
  value is checked again. A mismatch that is not explained by the kernel clamping to a lowered
  maximum counts as contention: every floor is restored and the controller enters
  `SAFE_FALLBACK` for `fault_backoff_ms`.
- **Thermal precedence.** The thermal cap is applied in the controller and again in the lease
  manager. At SEVERE and above (Android thermal status 3 or higher), boosting is suspended,
  and configuration validation rejects any cap above 0 for those statuses. Recovery is
  gradual.
- **Bottleneck-aware.** The CPU is raised only for `CpuBound`. For `Unknown` the cap is
  `unknown_max_level`. For GPU-bound, composition-late, app-wait or thermally capped
  frames, the level is 0.

## Controller states

| State | Meaning | Boost |
|---|---|---|
| `disabled` | FAS or this app disabled | 0 |
| `initializing` | before the first tick | 0 |
| `warmup` | fewer than `warmup_frames` or `min_samples` | 0 |
| `paused` | no frame for `idle_timeout_ns` (menu, loading, background) | 0 |
| `normal` | meeting deadlines | 0 |
| `degraded` | misses ≥ `degrade_miss_ratio`, waiting for evaluation, cooldown, or a CPU-relevant bottleneck | held |
| `boost_short` | burst of `consecutive_trigger` misses | `short_level` for `short_boost_ms` |
| `boost_sustained` | degradation persisted `eval_ticks` | stepped up after each `cooldown_ms`, up to the cap |
| `recovery` | stable (≤ `recover_miss_ratio`) or `max_boost_ms` reached | stepped down |
| `thermal_limited` | thermal suspension, or held at the thermal cap | capped or 0 |
| `telemetry_unavailable` | no valid frames from the backend | 0 |
| `safe_fallback` | write, read-back or contention fault | 0, for `fault_backoff_ms` |

ADAPTIVE additionally reverts a step that did not reduce misses within `eval_ticks` and
blocks further steps for `max_boost_ms`.

## Lifecycle and failure handling

| Event | Handling (code) |
|---|---|
| Start | `restore_leftovers` replays `restore.json` from a crashed run; refuses to start if that fails (`daemon.rs`) |
| First write to a policy | the original `scaling_min_freq` is saved to `restore.json` (atomic) **before** the write (`cpufreq.rs`) |
| App switch | leases released, analyzer, target and CPCS reset |
| SIGTERM / SIGINT | loop exits, `Engine::shutdown` restores, then `restore_leftovers` |
| SIGHUP / `ctl reload` | the new config is validated; an invalid config is rejected and the old one kept |
| Panic | `catch_unwind` in `daemon::run`, then file-based restore (`panic = "unwind"` in the release profile) |
| Loop stall over 15 s | the watchdog thread restores from file and aborts; `service.sh` restarts with backoff |
| Daemon exit | `service.sh` runs `zfasd restore`, then backs off 5 → 300 s |
| Uninstall | `uninstall.sh` stops the daemon, runs `zfasd restore`, and removes the data directory |
| Reboot | cpufreq floors are not persistent; the kernel starts from its defaults |

## Files

| Path (device) | Content | Writer |
|---|---|---|
| `/data/adb/zairenkai/fas/config.toml` | user config (versioned, partial) | user, `zfasd ctl` (atomic) |
| `/data/adb/zairenkai/fas/restore.json` | original node values while a floor is held | `CpufreqBackend` (atomic, 0600) |
| `/data/adb/zairenkai/fas/state.json` | status snapshot, refreshed every 2 s | daemon (atomic) |
| `/data/adb/zairenkai/fas/zfasd.sock` | control socket (0600, SO_PEERCRED root or same uid) | daemon |
| `/data/adb/zairenkai/fas/zfasd.log` | log, rotated at 512 KiB | `service.sh` |

## Trust boundaries

- The daemon runs as root. Its inputs are kernel sysfs, `dumpsys` output, a root-only config
  file, and a root-only socket.
- The socket accepts a fixed command set (`socket.rs::parse_request`), with no arguments other
  than a profile name from a fixed list.
- No input is ever passed to a shell. Commands are fixed argv vectors. The layer name passed
  to `dumpsys` comes from SurfaceFlinger's own list and is filtered by a validated package name.
- The restore file is treated as untrusted input. Only `cpufreq/policyN/scaling_min_freq` paths
  with integer values are honoured (`cpufreq::restorable`).
- Logs and the state file contain the foreground package and layer name and nothing else
  about apps: no window contents, no user data.
