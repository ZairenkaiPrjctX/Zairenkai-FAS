# Testing, validation and benchmarks

## What is tested where

| Layer | Tests | Runs on |
|---|---|---|
| Analyzer: percentiles, gaps, duplicates, out-of-order, mixed clocks, idle, target change | `zfas-core/src/analyzer.rs` | host, CI |
| Controller: single slow frame ignored, cooldown, caps, GPU-bound no CPU boost, unknown bounded, recovery to 0, time bound, thermal, faults, burst, ADAPTIVE revert, low confidence | `zfas-core/src/controller.rs` | host, CI |
| Thermal governor, profiles, config (migration, validation, rejection of unsafe values), target selection, leases (expiry, conflict, failure restore, contention), CPCS (chain attribution, fallback, instability), engine scenarios | `zfas-core/src/*.rs` | host, CI |
| cpufreq discovery, floor math, write and read-back, restore, crash-file restore allowlist, thermal clamp vs. contention, missing-node safety | `zfas-platform/src/cpufreq.rs` (fixture root) | host, CI |
| SurfaceFlinger parsing and layer choice, foreground parsing, `/proc` stat parsing, thermal, kgsl, tracefs line parsing and instance lifecycle, probe, atomic write, command timeout | `zfas-platform/src/*.rs` | host, CI |
| Socket protocol and permissions, trace round trip, synthetic scenarios through replay, daemon lifecycle (crash leftovers restored, invalid config refused), **end-to-end**: scripted 48 FPS telemetry boosts a fixture cpufreq tree within caps and restores it on exit | `zfasd/src/*.rs` | host, CI |
| Shell scripts | shellcheck | CI |
| Android arm64 build and package | `scripts/build-android.sh` | CI |

No test writes real device frequencies or mutates the host.

## Synthetic scenarios (replay)

```sh
zfasd synth cpu_drop /tmp/cpu_drop.jsonl
zfasd replay [--config cfg.toml] [--timeline] /tmp/cpu_drop.jsonl
```

Synthetic traces are labelled `origin = "synthetic"`, and every frame is tagged `synthetic`.
They show controller behaviour; they are not device measurements. Results from 2026-10-19
(release build, `target_fps = [60]`, 500 ms ticks):

| Scenario | Max level per cluster | Final state | Notes |
|---|---|---|---|
| steady60 | 0, 0, 0 | normal | no boost |
| cpu_drop (48 FPS, CPU busy 0.95, for 25 s) | 6, 6, 6 (balanced cap) | normal | boosted, then fully released after recovery |
| thermal (MODERATE then SEVERE) | 6 before heat | – | 34 s in `thermal_limited`, cap enforced, gradual recovery |
| pause (no frames for 10 s) | – | – | 9 s `paused` with 0 boost, re-warmup after resume |

## Engine overhead benchmark

`zfasd bench [SCENARIO] [ROUNDS]` measures host CPU time per `Engine::step` on synthetic input.
It excludes `dumpsys` and sysfs I/O.

Measured 2026-10-19 on an Intel Xeon @ 2.80 GHz (4 vCPU, container), `cpu_drop`, 50 rounds:
mean 1.28 µs, p99 2.52 µs, max 162 µs per step.

This is **not** a device FPS or power figure. On a device, the daemon reports its real loop
cost (including `dumpsys`) as `loop_ms_avg` and `loop_ms_max` in `state.json`.

## Device validation procedure (not yet performed)

None of the following has been carried out for this release. Until it has, no FPS or battery
claim may be made.

1. **Probe:** run `zfasd probe` and save the output with the device model, SoC, kernel and ROM.
2. **Observe:** run with `general.mode = "observe"` for 30 minutes of the target game. Check:
   - `state.json` shows `diag.telemetry_source` = `surface_flinger_latency`;
   - `fps` matches the game's counter or `dumpsys gfxinfo` within 1 FPS;
   - there are no `telemetry_unavailable` stretches during gameplay;
   - `loop_ms_avg` is under 50 ms.
3. **Safety on device:**
   - While a boost is held, `kill -9` the daemon. Confirm `service.sh` runs `restore` and that
     every `scaling_min_freq` equals the value saved before the run.
   - Uninstall while boosted, and confirm the same.
   - Raise the device's thermal status (sustained load), and confirm the boost is 0 at SEVERE.
4. **A/B measurement:**
   - Same game, scene, brightness, refresh rate and ambient temperature, starting from the same
     battery temperature (±1 °C).
   - Run each configuration at least 3 times, 15 minutes each, alternating:
     (a) module disabled, (b) `balanced`, (c) `performance`.
   - Record a per-frame trace (`zfasd record 900 run.jsonl`, plus Perfetto FrameTimeline if
     available) and the battery current from `/sys/class/power_supply/battery/current_now` at
     1 Hz.
   - Report median FPS, 1% low, P95/P99 frame time, jank count, average power and peak
     temperature, each with its spread.
5. **ADAPTIVE:** keep it opt-in until step 4 shows it is no worse than `balanced` on at least
   three devices from different SoC vendors.
6. **CPCS:** compare `cpcs.enabled = true` against `false` with the step 4 procedure. Also
   record how much of the time CPCS falls back (`cpcs.fallback` in `state.json`).
