# Third-party components and provenance

## Project licence

**Apache-2.0** (maintainer decision, 2026-10-21; as for Zairenkai-Core and -ThermalTools):
`LICENSE` (the official text), `NOTICE`, SPDX headers in every Rust, shell and HTML file, and
`license = "Apache-2.0"` in Cargo. The copyright line follows the organization convention,
"Copyright 2026 ZairenkaiPrjctX contributors". The component (`dist/zairenkai-fas/`)
carries `LICENSE`, `NOTICE` and this file into the `zairenkai` module zip.

The decision followed a provenance audit (Zairenkai-App `docs/integration/fas-provenance.md`,
re-run 2026-10-21): no run of 3 or more lines matches fas-rs `09ba876`, fas-rs-cpcs `aa3583a` or
encore_fas `f408a00`, the single matching lines are boilerplate, and every dependency below is
permissive. Every licence in the tables below is compatible with Apache-2.0.

### Authorship

Checked in `git log`, 2026-10-21:
- **Non-merge commits** (9 up to `9d87236`, and every later one): all were made by Claude Code sessions (author "Claude
  <noreply@anthropic.com>") run by the maintainer, GitHub `FebriCahyaa`. Every one except the
  first agent-foundation commit carries the maintainer's `Signed-off-by`.
- **Merges (2):** the maintainer's merges of PR #1 and PR #2.
- **Other contributors:** none.

Anthropic's terms assign to the user whatever rights Anthropic has in Outputs:
- Consumer Terms, effective 2025-10-08, §4: "we assign to you all of our right, title, and
  interest—if any—in Outputs", subject to compliance with the terms.
- Commercial Terms, effective 2025-06-17, §B: the customer "owns its Outputs".

The copyright line "ZairenkaiPrjctX contributors" refers to the contributors in the history. It
names no legal entity and claims no exclusive ownership.

**Not settled:** whether, and to what extent, copyright subsists in AI-assisted code depends on
the jurisdiction and on the human contribution. Apache-2.0 is granted to the extent the
contributors hold rights. The provenance audit is evidence, not proof, of independent authorship:
it compared lines with three projects, so paraphrase or other sources cannot be ruled out. This
record is not legal advice.

## Rust dependencies compiled into `zfasd`

These come from crates.io. `Cargo.lock` pins the exact versions, and CI builds with `--locked`.

| Crate | Version | Licence | Used for |
|---|---|---|---|
| serde, serde_core, serde_derive | 1.0.229 | MIT OR Apache-2.0 | (de)serialization |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | state file, traces, control replies |
| toml, toml_parser, toml_writer, toml_datetime, serde_spanned | 1.1.x | MIT OR Apache-2.0 | configuration |
| winnow | 1.0.4 | MIT | TOML parsing (via toml) |
| indexmap, hashbrown, equivalent | 2.14.2 / 0.17.1 / 1.0.3 | MIT OR Apache-2.0 | TOML tables (via toml) |
| libc | 0.2.191 | MIT OR Apache-2.0 | `clock_gettime`, `sysconf`, `SO_PEERCRED`, signals |
| itoa, zmij, memchr | 1.0.18 / 1.0.23 / 2.8.3 | MIT OR Apache-2.0 / MIT / Unlicense OR MIT | number formatting, search (via serde_json) |
| proc-macro2, quote, syn, unicode-ident | build time only | MIT OR Apache-2.0 (unicode-ident also Unicode-3.0) | `serde_derive` |

To regenerate this list, run `cargo metadata --format-version 1 --filter-platform aarch64-linux-android`.

## Build tools (not redistributed)

| Tool | Version | Licence | Notes |
|---|---|---|---|
| Android NDK | r27d | Android SDK licence | Downloaded by CI and checked against SHA-256 `601246087a682d1944e1e16dd85bc6e49560fe8b6d61255be2829178c8ed15d9`. Only its linker is used. |
| Rust toolchain | 1.97.0 (CI), 1.85.0 (MSRV) | MIT OR Apache-2.0 | |

## Reference projects (studied, not copied)

These projects were read to learn what problems exist and how they have been approached.
**No code from them was copied, ported or adapted.** Zairenkai-FAS is written independently
in a different architecture: SurfaceFlinger telemetry instead of uprobes, boost levels
applied through expiring leases, and a separate state machine. GPL-licensed code must not be
merged into this repository unless the project licence is made compatible.

| Project | Revision read | Licence | What informed the design |
|---|---|---|---|
| [encore_fas](https://github.com/rem01project/encore_fas) | `f408a00` (2026-10-03) | GPL-2.0 (UAPI header GPL-2.0-only WITH Linux-syscall-note) | Pause detection; acquisition period before judging; telemetry-only kernel side; the need to handle kernel API drift. Not used: kernel module, CUSUM detector. |
| [fas-rs](https://github.com/shadow3aaa/fas-rs) | `09ba876` (v4.9.1, 2026-07-17) | GPL-3.0 | The target-FPS ladder idea ("the lowest rate not clearly exceeded"); fixed-margin thinking; what goes wrong without restore-on-crash (fas-rs restores only on disable or game switch). Not used: eBPF frame source, `scaling_max_freq` writes, disabling vendor boosters, Lua extensions. |
| [fas-rs-cpcs](https://github.com/shadow3aaa/fas-rs-cpcs) | `aa3583a` (2026-03-03) | GPL-3.0; its BPF object is "Dual MIT/GPL" | That critical-path weights need a confidence value and a staleness fallback. Not used: eBPF probes, futex edges, budget water-filling, constants. |
| CPCS blog post | fetched 2026-10-19 | n/a | The concept: attribute a frame's critical chain to CPU clusters. The post publishes no formulas or constants. |
| Android Performance Hint API, Perfetto FrameTimeline docs | public documentation | Apache-2.0 docs | Frame-timing vocabulary (desired / actual present, ready). |

Where Zairenkai-FAS deliberately differs from the references:

- **Floor only.** It writes `scaling_min_freq` and never `scaling_max_freq`, so it can never raise
  or remove a thermal or vendor maximum.
- **No disabling of vendor performance agents** (fas-rs disables MTK FPSGO and Xiaomi MIGT).
  Zairenkai-FAS detects them and backs off on contention instead.
- **No system properties are changed.** fas-rs sets `debug.sf.latch_unsignaled` and frame-pacing
  props.
- **Restore on every exit path**: signal, panic, watchdog stall, the `zfasd restore` startup step,
  and uninstall.

## Magisk installer interface

The `update-binary` stub, now in Zairenkai-Modules, is an independently written, minimal
script. It calls Magisk's documented module installer interface (`util_functions.sh`,
`install_module`) and contains no Magisk code.
