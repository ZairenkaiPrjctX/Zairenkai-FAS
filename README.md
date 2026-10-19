# Zairenkai-FAS

Frame-aware scheduling for rooted Android (Magisk / KernelSU / APatch).

`zfasd` watches how the foreground app's frames reach the display. When they keep missing
their deadline **and the CPU is the bottleneck**, it raises CPU frequency floors in small,
time-limited, thermally capped steps. As soon as there is no longer a reason to boost, it puts
everything back.

> **Status (0.1.0):** implemented and tested on the host against fixtures, cross-built for
> arm64. **Not yet validated on a physical device**, and no FPS or battery result has been
> measured. Use observe mode first (see below). `docs/TESTING.md` describes the validation
> procedure.

## What it does and does not do

| Does | Does not |
|---|---|
| Read frame timing from SurfaceFlinger (`dumpsys SurfaceFlinger --latency`) | Fabricate or estimate FPS when telemetry is missing (it releases boosts instead) |
| Raise `scaling_min_freq` floors within `[cpuinfo_min, current scaling_max]` | Touch `scaling_max_freq`, governors, thermal nodes or vendor tunables |
| Classify the bottleneck: CPU, GPU, app/sync wait, composition, thermal/power, unknown | Boost the CPU for GPU-bound or composition-late frames |
| Cap boosts by Android thermal status; suspend at SEVERE and above | Bypass or weaken thermal protection |
| Save originals before writing, verify by read-back, expire leases, restore on any exit | Leave floors behind after stop, crash, stall or uninstall |
| Detect other agents changing its values, then back off | Disable vendor performance services |
| Offer experimental CPCS cluster weighting (off by default) | Claim CPCS gains (none measured) |

## Quick start

```sh
# Build (host tests, then the Android module)
cargo test --workspace
ANDROID_NDK_HOME=/path/to/android-ndk-r27d scripts/build-android.sh

# Install dist/zairenkai-fas-v*.zip in your root manager and reboot. Then:
su -c /data/adb/modules/zairenkai_fas/bin/zfasd probe        # capabilities, read-only
su -c /data/adb/modules/zairenkai_fas/bin/zfasd ctl status   # live state
```

Observe mode makes decisions without writing anything. Put this in
`/data/adb/zairenkai-fas/config.toml`:

```toml
[general]
mode = "observe"
```

## Profiles

`power_saver`, `balanced` (default), `performance`, `fast_response`, `adaptive` (experimental;
reverts boost steps that do not help). Switch with `zfasd ctl profile performance`, or set
one per app:

```toml
[apps."com.example.game"]
profile = "performance"
target_fps = [60]
```

## Commands

```
zfasd run [--observe-only]      daemon (started by service.sh)
zfasd probe                     device capability report (JSON)
zfasd ctl status|enable|disable|reload|reset|profile NAME
zfasd restore                   put back values left by a crashed run
zfasd record SECONDS OUT        record live observations (no control)
zfasd replay [--config F] TRACE run a trace through the real engine offline
zfasd synth SCENARIO OUT        labelled synthetic trace
zfasd bench                     engine overhead (host, synthetic)
zfasd check-config FILE
```

## Documentation

- [Architecture](docs/ARCHITECTURE.md): data flow, state machine, lifecycle, trust boundaries
- [Capabilities](docs/CAPABILITIES.md): backend matrix, what is and is not implemented
- [Configuration](docs/CONFIGURATION.md): every key, defaults and validation
- [Build and install](docs/BUILD_AND_INSTALL.md)
- [Testing and benchmarks](docs/TESTING.md): tests, synthetic results, device procedure
- [Integration](docs/INTEGRATION.md): socket protocol and state file for UIs
- [Third party and provenance](docs/THIRD_PARTY.md): dependencies and the references studied (no code copied)

## Licence

Not chosen yet; see `docs/THIRD_PARTY.md`.
