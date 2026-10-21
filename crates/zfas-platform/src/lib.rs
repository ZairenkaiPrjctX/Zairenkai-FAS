// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 ZairenkaiPrjctX contributors
//! Zairenkai-FAS platform adapters.
//!
//! Each module wraps one Android or Linux interface, parses it defensively, and reports
//! problems instead of guessing. All file access goes through [`sysroot::SysRoot`] so tests
//! run against fixtures and never touch the host's device nodes.

pub mod cpufreq;
pub mod exec;
pub mod foreground;
pub mod fsutil;
pub mod gpu;
pub mod probe;
pub mod procstat;
pub mod surfaceflinger;
pub mod sysroot;
pub mod testutil;
pub mod thermal;
pub mod tracefs;
