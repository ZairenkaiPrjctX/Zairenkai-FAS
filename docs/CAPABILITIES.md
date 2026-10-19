# Capabilities and backend matrix

To see what a specific device supports, run `zfasd probe` as root. It only reads.

Status words used below:
- **Implemented, tested** – code exists and host tests cover it against fixtures.
- **Not physically validated** – nothing in this release has been run on a real device yet.
  "Implemented" never means "validated on hardware".
- **Not implemented** – probed and reported only.

## Telemetry (frame source)

| Backend | Status | Requirements | Notes |
|---|---|---|---|
| SurfaceFlinger `--latency` | Implemented, tested; not physically validated | root (or shell), `dumpsys` | Present, ready and desired-present times in `CLOCK_MONOTONIC`. The ~128-frame ring is polled every tick and de-duplicated. Layer chosen from `--list`: SurfaceView/BLAST first. |
| Replay (JSONL trace) | Implemented, tested | none | `zfasd record` (live, observe-only) and `zfasd synth` (synthetic; frames tagged `synthetic`). |
| eBPF uprobe on `Surface::queueBuffer` | Not implemented | BTF (`/sys/kernel/btf/vmlinux`), uprobes, kernel ≥ 5.8 for ring buffer | Probe reports prerequisites only. Planned. |
| Kernel module | Not implemented | vendor kernel headers | Planned; no module ships. |
| Fallback observer | Implemented | none | No telemetry means `telemetry_unavailable`: boosts are released, never guessed. |

## Evidence for bottleneck classification

| Evidence | Source | Status |
|---|---|---|
| Max thread busy | `/proc/<pid>/task/*/stat` (utime+stime), parsed from the last `)` | Implemented, tested |
| GPU busy | `/sys/class/kgsl/kgsl-3d0/gpu_busy_percentage` (Qualcomm) | Implemented, tested; other vendors unsupported (not guessed) |
| CPU max capped | `scaling_max_freq < cpuinfo_max_freq` per policy | Implemented, tested |
| Composition late | SurfaceFlinger: buffer ready before desired present, presented more than one period late | Implemented, tested |

Classification is always an estimate (`Provenance::Estimated`). The thresholds are wide on
purpose, and anything not clearly attributable becomes `unknown`, which gets the conservative
`unknown_max_level` cap.

## Control

| Backend | Status | Writes | Notes |
|---|---|---|---|
| cpufreq floor | Implemented, tested on fixtures; not physically validated | `policyN/scaling_min_freq` only | Value in `[cpuinfo_min, current scaling_max]`, snapped down to `scaling_available_frequencies`. Read back after every write; original saved before the first write. |
| GPU devfreq floor | Not implemented | none | The controller's `gpu_level` is always 0 in this release. |
| Memory (observe / replay) | Implemented, tested | none | Used for `mode = "observe"`, `--observe-only`, replay, bench. |

## Thermal

| Input | Status | Notes |
|---|---|---|
| Android thermal status (`dumpsys thermalservice`, `Thermal Status: N`) | Implemented, tested | Primary input. Vendor-defined thresholds. Unknown or unreadable status: conservative cap (`unknown_cap`, default 4). |
| Raw thermal zones | Implemented, tested | Used only with thresholds configured per zone `type`. None are built in. |
| CPU max capping | Implemented, tested | Classified as `thermal_or_power`, so no CPU boost. |

Zairenkai-FAS never writes thermal nodes and never disables or bypasses thermal
protection.

## CPCS (critical-path cluster scaling)

| Item | Status |
|---|---|
| Source | tracefs instance `zairenkai_fas`: `sched_switch` + `sched_waking` (falls back to `sched_wakeup`), `trace_clock = mono` |
| Analyzer | Implemented, tested on synthetic chains; **experimental**, `cpcs.enabled = false` by default |
| Output | Per-domain weights with confidence (path stability × coverage); uniform fallback below `min_confidence` or with fewer than `min_frames` frames |
| Physically validated | No |

## Conflicts with other agents

Vendor performance agents (MediaTek FPSGO and perfmgr, Xiaomi MIGT, the Qualcomm perf HAL)
and other FAS modules may write the same nodes. The probe lists the ones it can detect.
Zairenkai-FAS does **not** disable them. Contention is detected at runtime by read-back
verification, after which it restores and backs off. Running two FAS modules at once is not
supported.

## Platform requirements

- arm64, Android 10 (API 29) or newer (`customize.sh` enforces both).
- Root via Magisk (v20.4+), KernelSU or APatch. WebUI requires KernelSU (or a manager that
  provides the same `ksu.exec` API).
