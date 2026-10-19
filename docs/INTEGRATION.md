# Integration contract (UI and other components)

No Zairenkai app exists yet (on 2026-10-19 `Zairenkai-App` branch `dev` contained only the agent foundation files). Any UI, whether the
bundled KernelSU WebUI or a future app, integrates only through the interfaces below. The UI
never runs arbitrary commands and never writes cpufreq nodes.

## Control socket

`/data/adb/zairenkai-fas/zfasd.sock` is a Unix stream socket, mode 0600. Peers must run as
root or as the daemon's uid; this is checked with `SO_PEERCRED`.

Send one request line per connection and read one JSON line back.

| Request | Effect | Persisted |
|---|---|---|
| `status` | full snapshot (same schema as `state.json`) | – |
| `enable` / `disable` | master switch | yes (config, atomic) |
| `profile <power_saver\|balanced\|performance\|fast_response\|adaptive>` | default profile | yes |
| `reload` | re-read and validate `config.toml`; rejected if invalid | – |
| `reset` | release boosts and clear history now | – |

Replies are `{"ok":true,"message":...}` or `{"ok":false,"error":...}`. From a shell:
`zfasd ctl <request>`.

## State file

`/data/adb/zairenkai-fas/state.json` is rewritten atomically every 2 s, and once more with
`"running": false` at exit. Schema version 1. Main fields:

| Field | Meaning | Provenance |
|---|---|---|
| `running`, `pid`, `updated_mono_ms` | liveness (compare with `CLOCK_MONOTONIC`) | – |
| `mode`, `observe_only` | whether writes happen | – |
| `enabled`, `default_profile` | `general.enabled` and `general.profile` from the configuration | configured |
| `package`, `layer`, `app_enabled`, `profile` | foreground app and its effective settings (`profile` may differ from `default_profile` through a per-app override) | measured |
| `step.stats.fps`, `p50_ns`, `p95_ns`, `p99_ns`, `miss_ratio`, `samples`, `confidence` | frame statistics | measured (SurfaceFlinger) |
| `step.target_fps` | selected target | derived |
| `step.decision.state`, `reason`, `cpu_level`, `bottleneck` | controller output; the bottleneck is an estimate | estimated |
| `step.thermal` | cap and reason | measured status, derived cap |
| `step.cpcs` | weights, confidence, `fallback`, `provenance` | estimated |
| `domains[].level` | level in effect per cpufreq policy | applied (or simulated in observe mode) |
| `diag` | time per state, boost steps, control errors, frames, misses | – |
| `loop_ms_avg`, `loop_ms_max` | daemon's own cost per tick | measured |
| `errors` | last 8 warnings | – |

A UI must label estimated values as estimates. It must show "observe only" when
`observe_only` is true. It must not show a FPS value when `telemetry_error` is set.
