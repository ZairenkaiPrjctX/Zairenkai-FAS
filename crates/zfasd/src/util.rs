//! Clock, logging and signal plumbing.

use std::sync::atomic::{AtomicBool, Ordering};

/// `CLOCK_MONOTONIC` in ns: the same domain as SurfaceFlinger timestamps and the tracefs
/// `mono` clock.
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid pointer to a timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

pub fn monotonic_ms() -> u64 {
    monotonic_ns() / 1_000_000
}

#[macro_export]
macro_rules! log {
    ($lvl:expr, $($arg:tt)*) => {{
        let ms = $crate::util::monotonic_ms();
        eprintln!("[{}.{:03}] {} {}", ms / 1000, ms % 1000, $lvl, format_args!($($arg)*));
    }};
}

pub static STOP: AtomicBool = AtomicBool::new(false);
pub static RELOAD: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

extern "C" fn on_reload(_: libc::c_int) {
    RELOAD.store(true, Ordering::SeqCst);
}

pub fn install_signal_handlers() {
    // SAFETY: the handlers only store to atomics, which is async-signal-safe.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGHUP,
            on_reload as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}
