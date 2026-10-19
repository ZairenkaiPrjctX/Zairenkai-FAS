# Configuration reference

File: `/data/adb/zairenkai-fas/config.toml`. When it is missing, the built-in defaults apply
(`zfasd default-config` prints them).

- **Partial files are fine.** Write only the keys you change; everything else comes from the
  compiled defaults.
- **Unknown keys are errors.** A typo is rejected; it is never silently ignored.
- **Validate before applying.** Run `zfasd check-config FILE`. A running daemon rejects an
  invalid file on `SIGHUP` or `zfasd ctl reload` and keeps the previous configuration.
- **Writes are atomic.** `zfasd ctl profile|enable|disable` rewrite the file through a temp
  file, `fsync` and `rename`. The rewritten file contains every setting, and comments are not
  kept.
- **Versioned.** `schema_version` (currently 1).
  - Older files are migrated step by step (`config.rs::MIGRATIONS`).
  - Files from a newer version are refused.
  - A missing version is treated as the current one and noted.

## `[general]`

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Master switch. `false` releases all boosts (state `disabled`). |
| `mode` | `"control"` | `"observe"` makes decisions and reports them, but never writes a node. |
| `scope` | `"all"` | `"all"`: every foreground app except those with `enabled = false`. `"listed"`: only apps under `[apps]`. |
| `profile` | `"balanced"` | Default profile: `power_saver`, `balanced`, `performance`, `fast_response`, `adaptive`. ADAPTIVE is experimental and stays opt-in until it is validated on hardware. |
| `tick_ms` | `500` | Controller period, 100..=5000. Leases last 3 ticks. |

## `[control]`

| Key | Default | Meaning |
|---|---|---|
| `cpu` | `true` | Use cpufreq floors. `false` behaves like observe mode. |
| `exclude_policies` | `[]` | cpufreq policy directories never touched, e.g. `["policy0"]`. |
| `verify_every_ticks` | `6` | Re-read written values this often. A foreign change counts as contention: restore and back off. |
| `fault_backoff_ms` | `30000` | After a write, read-back or contention fault, stay in `safe_fallback` this long. |

## `[analyzer]`

| Key | Default | Meaning |
|---|---|---|
| `window_frames` | 240 | Frames in the statistics window (≤ 4096). |
| `min_samples` | 30 | Frames before P95 and confidence are trusted (≥ 10). P99 needs 100. |
| `deadline_tolerance` | 0.1 | A frame misses when its interval is over `target_period × 1.1`. |
| `ewma_alpha` | 0.1 | Smoothing of the interval EWMA. |
| `idle_timeout_ns` | 1.5 s | No frame for this long means `paused`. |
| `max_interval_ns` | 0.5 s | A longer interval is a gap (pause, event loss): timing restarts, nothing is recorded. |
| `warmup_frames` | 60 | Frames after a (re)start before acting. |

## `[target]`

The target is the lowest candidate rate the observed FPS does not clearly exceed (by
`margin_fps`), and never above the display refresh rate. Candidates are the app's
`target_fps` list, or `[30, 45, 60, 90, 120, 144]` by default.

| Key | Default | Meaning |
|---|---|---|
| `margin_fps` | 3.0 | Observed FPS may be this far above a candidate and still count as aiming for it. |
| `up_after` | 2 | Consecutive one-second evaluations needed to raise the target. |
| `down_after` | 6 | Consecutive evaluations needed to lower it. |

> Without a per-app list, a game that falls from 60 to about 44 FPS is eventually judged as
> aiming for 45, and stops being boosted. For games you care about, set
> `target_fps = [60]` (or the rate you choose in the game).

## `[thermal]`

| Key | Default | Meaning |
|---|---|---|
| `level_cap_by_status` | `[10, 6, 3, 0, 0, 0, 0]` | Highest level per Android thermal status (NONE, LIGHT, MODERATE, SEVERE, CRITICAL, EMERGENCY, SHUTDOWN). Must not increase with heat; SEVERE and hotter **must** be 0 (validation enforces this). |
| `unknown_cap` | 4 | Cap when the thermal status cannot be read. |
| `recovery_ms` | 10000 | Cooling time before a lowered cap rises by 2 levels. |
| `zones` | `[]` | Optional raw-sensor limits: `{ zone_type = "...", derate_mc = 80000, suspend_mc = 90000 }`. No thresholds are built in; use values from your device's vendor thermal configuration. |

## `[cpcs]` (experimental)

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Start the tracefs scheduler instance and use critical-path weights. |
| `min_confidence` | 0.6 | Below this confidence, weights are uniform (same as CPCS off). |
| `min_frames` | 16 | Frames analyzed before non-uniform weights are used. |
| `min_coverage` | 0.25 | Chains covering less of the frame time are weaker evidence. |
| `alpha` | 0.2 | EMA of per-cluster shares. |
| `floor` | 0.2 | Lowest relative weight a cluster gets while CPCS is active. |

## `[profiles.<name>]`

Override any of these per profile: `max_level`, `unknown_max_level`, `short_level`,
`step_up`, `step_down`, `degrade_miss_ratio`, `recover_miss_ratio`, `consecutive_trigger`,
`eval_ticks`, `cooldown_ms`, `short_boost_ms`, `max_boost_ms`, `recovery_ms`,
`min_confidence`.

Validation rules:
- levels ≤ 10;
- `recover_miss_ratio` < `degrade_miss_ratio` (hysteresis);
- `consecutive_trigger` ≥ 2, so one slow frame is never enough;
- `max_boost_ms` ≤ 120000.

Built-in values (`policy.rs`):

| Profile | max | unknown max | short | step up | degrade / recover | trigger | eval ticks | cooldown | max boost | recovery |
|---|---|---|---|---|---|---|---|---|---|---|
| power_saver | 3 | 1 | 2 | 1 | 0.20 / 0.08 | 8 | 5 | 3 s | 10 s | 2 s |
| balanced | 6 | 3 | 3 | 1 | 0.10 / 0.03 | 4 | 3 | 1.5 s | 20 s | 4 s |
| performance | 9 | 5 | 5 | 1 | 0.05 / 0.01 | 3 | 2 | 1 s | 30 s | 6 s |
| fast_response | 8 | 4 | 6 | 2 | 0.10 / 0.03 | 2 | 1 | 0.5 s | 20 s | 3 s |
| adaptive | 8 | 4 | 3 | 1 | 0.10 / 0.03 | 4 | 3 | 1.5 s | 20 s | 4 s, reverts ineffective steps |

A level is a fraction of each policy's range. Level 6 puts the floor at the published
frequency at or just below 60% of the way from `cpuinfo_min_freq` to the current
`scaling_max_freq`. It is never set below the floor that was already there.

## `[apps."<package>"]`

```toml
[apps."com.example.game"]
profile = "performance"   # optional
target_fps = [60, 120]    # optional; 10..=240
enabled = true            # optional; false excludes the app
```

Package names are validated (letters, digits, `_`, `.`).

## Example

```toml
schema_version = 1

[general]
profile = "balanced"

[control]
exclude_policies = ["policy0"]

[apps."com.example.game"]
target_fps = [60]

[apps."com.example.reader"]
enabled = false
```
