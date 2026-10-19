# Architecture: Zairenkai-FAS

> Owned by this repository. Document only what inspected code supports, citing file paths.

The full description is in `docs/ARCHITECTURE.md`. This page is a summary for agents.

## Overview

`zfasd` (`crates/zfasd/src/main.rs`) is a root daemon packaged as a Magisk/KernelSU module
(`module/`). Each tick (default 500 ms):
1. It reads SurfaceFlinger frame timestamps, per-thread CPU busy, kgsl GPU busy, cpufreq
   capping and the Android thermal status.
2. `zfas_core::engine::Engine::step` turns these into a bounded boost level.
3. The level is applied as expiring leases on cpufreq `scaling_min_freq` floors.

## Components

| Path | Responsibility |
|---|---|
| `crates/zfas-core/src/model.rs` | clock-domain timestamps, frame events, provenance |
| `crates/zfas-core/src/analyzer.rs` | window stats (EWMA, P50/P95/P99, misses, gaps, idle, confidence) |
| `crates/zfas-core/src/target.rs` | target-FPS selection with hysteresis |
| `crates/zfas-core/src/controller.rs` | state machine and bottleneck classification |
| `crates/zfas-core/src/policy.rs` | profiles in boost levels 0..=10 |
| `crates/zfas-core/src/thermal.rs` | thermal cap with gradual recovery |
| `crates/zfas-core/src/control.rs` | `ControlBackend` trait, `LeaseManager`, `MemoryBackend` |
| `crates/zfas-core/src/cpcs.rs` | experimental critical-path weights |
| `crates/zfas-core/src/config.rs` | versioned config, migration, validation |
| `crates/zfas-core/src/engine.rs` | one deterministic step (shared by daemon and replay) |
| `crates/zfas-platform/src/*` | device adapters behind `SysRoot` / `Runner` |
| `crates/zfasd/src/daemon.rs` | run loop, watchdog, restore, socket, state |

## Entry points

- `zfasd run|probe|restore|ctl|record|replay|synth|bench|check-config|default-config`
- `module/service.sh` (boot), `module/customize.sh` (install), `module/uninstall.sh`

## Trust boundaries

Root daemon. Inputs: sysfs, `dumpsys` output, a root-only config file, and a root-only Unix
socket with a fixed command set. The restore file is validated against an allowlist before
use. No shell execution. Details: `docs/ARCHITECTURE.md#trust-boundaries`.

## Generated artifacts

`dist/` (module zip) from `scripts/build-android.sh`. `module.prop` is generated from
`module/module.prop.in` and the workspace version; edit the template.
