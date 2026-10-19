//! Zairenkai-FAS core logic.
//!
//! This crate performs no I/O. Every input (frames, hardware evidence, thermal status,
//! clock) is passed in, so the whole control path is deterministic and testable on any
//! host. Platform access lives in `zfas-platform`; the daemon wiring lives in `zfasd`.

pub mod analyzer;
pub mod config;
pub mod control;
pub mod controller;
pub mod cpcs;
pub mod diag;
pub mod model;
pub mod policy;
pub mod target;
pub mod thermal;
