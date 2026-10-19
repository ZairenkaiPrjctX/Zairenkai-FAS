# Project context: Zairenkai-FAS

> Owned by this repository. Update it whenever verified facts change. Label every statement
> **Verified** (with `Evidence:`), **Unverified**, or **Unknown / Not Configured**.

## Identity

| Field | Value | Status |
|---|---|---|
| Repository | `ZairenkaiPrjctX/Zairenkai-FAS` | Verified. Evidence: GitHub API |
| GitHub description | Zairenkai Tweaks FAS kernel module | Verified. Evidence: GitHub API. Note: the implementation is a userspace root daemon in a Magisk/KernelSU module, not a kernel module (see `docs/CAPABILITIES.md`) |
| Visibility | private | Verified. Evidence: GitHub API |
| Default branch | `dev` | Verified. Evidence: `git remote show origin` 2026-10-19 |
| Purpose | Frame-aware scheduling: bounded, thermal-aware CPU frequency floors driven by SurfaceFlinger frame telemetry | Verified. Evidence: `crates/`, `docs/ARCHITECTURE.md` |
| Licence | none chosen yet (no `LICENSE` file) | Verified. Evidence: repository tree; `docs/THIRD_PARTY.md` |

## Contents

| Area | Location | Status |
|---|---|---|
| Rust workspace (MSRV 1.85, edition 2021) | `Cargo.toml`, `Cargo.lock` | Verified. Evidence: `cargo +1.85.0 test --workspace` passed 2026-10-19 |
| Core logic, no I/O | `crates/zfas-core` (analyzer, controller, policy, thermal, target, control leases, CPCS, config, diag, engine) | Verified. Evidence: 51 tests pass |
| Platform adapters | `crates/zfas-platform` (cpufreq, SurfaceFlinger, foreground, procstat, thermal, gpu, tracefs, probe, exec, fsutil, sysroot) | Verified. Evidence: 23 fixture tests pass |
| Daemon / CLI | `crates/zfasd` (run, probe, restore, ctl, record, replay, synth, bench, check-config) | Verified. Evidence: 8 tests incl. end-to-end daemon test |
| Module packaging | `module/` (customize, service, uninstall, action, update-binary, WebUI) | Verified. Evidence: shellcheck clean; `scripts/build-android.sh` produced a zip 2026-10-19 |
| CI | `.github/workflows/ci.yml` | Verified locally. Evidence: actionlint clean 2026-10-19. **Unverified** on GitHub (not run yet) |
| Docs | `README.md`, `docs/*.md` | Verified. Evidence: files present |

## Build, test, and run

| Step | Command | Status |
|---|---|---|
| Test | `cargo test --workspace` | Verified. Evidence: 82 tests passed (51 + 23 + 8), 2026-10-19 |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check` | Verified. Evidence: clean 2026-10-19 |
| Android build | `ANDROID_NDK_HOME=... scripts/build-android.sh` | Verified. Evidence: NDK r27d, aarch64 PIE for `/system/bin/linker64`, zip in `dist/`, 2026-10-19 |
| On-device run | `zfasd run` via `service.sh` | **Unverified**: never executed on an Android device |

## Key facts and limitations

- Frame telemetry: `dumpsys SurfaceFlinger --latency` only. eBPF and kernel-module backends are
  probe-only (Not Implemented). Evidence: `crates/zfas-platform/src/probe.rs`.
- Control: cpufreq `scaling_min_freq` floors only; GPU control is not implemented. Evidence:
  `crates/zfas-platform/src/cpufreq.rs`.
- CPCS: experimental, off by default; only validated on synthetic chains. Evidence:
  `crates/zfas-core/src/cpcs.rs` tests.
- No FPS or power result has been measured on a device. Evidence: `docs/TESTING.md`.

## Authoritative sources

| Topic | Source |
|---|---|
| Repository facts | this file |
| Architecture | `AGENT/ARCHITECTURE.md` → `docs/ARCHITECTURE.md` |
| Capabilities | `docs/CAPABILITIES.md` |
| Configuration | `docs/CONFIGURATION.md` |
| Licensing / provenance | `docs/THIRD_PARTY.md` |
| Organization standards | `ZairenkaiPrjctX/.github` → `agent-foundation/standards/` |
| Repository list and relationships | `ZairenkaiPrjctX/.github` → `agent-foundation/inventory/repositories.json` |

## Related repositories

- `Zairenkai-App`: future UI. It contained only agent foundation files on 2026-10-19
  (Evidence: `git ls-tree origin/dev`). The integration contract is `docs/INTEGRATION.md`.
- `Zairenkai-Database-Source` (AFID): thermal/power metadata that could later supply validated
  per-device thermal zone thresholds. **Unverified**: no integration exists.

## Open questions

- Which licence should the project use? It must be chosen before distribution.
- Which devices will be used for the validation procedure in `docs/TESTING.md`?
- Should the GitHub description be changed from "kernel module" to match the daemon design?
