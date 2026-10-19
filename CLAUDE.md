# CLAUDE.md: Zairenkai-FAS

Instructions for Claude Code in `ZairenkaiPrjctX/Zairenkai-FAS`. Keep this file under 200 lines;
put detail in `AGENT/`.

@AGENTS.md
@AGENT/PROJECT_CONTEXT.md

## Current state (read first)

This repository is **Zairenkai-FAS**: a Rust workspace (`crates/zfas-core`, `crates/zfas-platform`,
`crates/zfasd`) plus a Magisk/KernelSU module (`module/`). Architecture: `docs/ARCHITECTURE.md`.
Nothing has been validated on a physical device yet (`docs/TESTING.md`).

Non-negotiables in this repo:
- Only cpufreq `scaling_min_freq` floors are written, within `[cpuinfo_min, current scaling_max]`.
  Never write `scaling_max_freq`, governors, thermal nodes, or guessed vendor paths.
- Every write path must keep restore-before-write, read-back, lease expiry, and restore on exit.
- Thermal SEVERE and above means no boost. Keep that enforced in config validation.
- Tests use `SysRoot` fixtures and `FakeRunner`. They must never touch the host's `/sys` or `/proc`.
- Do not copy code from GPL references (encore_fas, fas-rs, fas-rs-cpcs); see `docs/THIRD_PARTY.md`.
- No FPS or battery claim without the measurement procedure in `docs/TESTING.md`.

## Commands

| Purpose | Command | Status |
|---|---|---|
| Build | `cargo build --workspace` | Verified. Evidence: 2026-10-19 |
| Test | `cargo test --workspace` | Verified. Evidence: 82 passed, 2026-10-19 |
| Lint / format | `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check` | Verified. Evidence: clean, 2026-10-19 |
| MSRV | `cargo +1.85.0 test --workspace` | Verified. Evidence: passed, 2026-10-19 |
| Shell lint | `shellcheck -s sh module/*.sh module/META-INF/com/google/android/update-binary && shellcheck scripts/*.sh` | Verified. Evidence: clean (0.10.0), 2026-10-19 |
| Android module | `ANDROID_NDK_HOME=<ndk> scripts/build-android.sh` | Verified. Evidence: NDK r27d, 2026-10-19 |
| Validate agent foundation | `python3 <path-to-.github>/agent-foundation/scripts/validate_agent_setup.py .` | Configured |

Only list commands that exist in this repository. Never invent one.

## Working rules

- Inspect the relevant code before editing it. Preserve unrelated and uncommitted changes.
- Generated files are edited at their source, then regenerated. Managed foundation files
  (listed in `AGENT/.foundation-manifest.json`) are edited in `ZairenkaiPrjctX/.github`, not here.
- Destructive or outward-facing actions (deleting data, force-push, history rewrites,
  publishing releases, deploying, flashing devices) need explicit human approval each time.
- Do not claim a build, test, or signature succeeded without showing the output.

## Git

- Work on a topic branch; open **draft** pull requests; never merge or approve.
- Commits must be cryptographically signed. If signing fails, stop and report the error.
  Trailer format and verification: `AGENT/policies/git-and-commit-signing.md`.

## Where things are

- `AGENT/README.md`: index of the agent library (prompts, policies, playbooks, templates).
- `.claude/skills/`: run `/repo-audit`, `/implement-feature`, `/bug-fix`, `/test-and-verify`,
  `/security-review`, `/release-readiness`.
- `.claude/rules/`: path-scoped rules, loaded automatically.
