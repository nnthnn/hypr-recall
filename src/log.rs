//! Minimal verbosity control. High-level progress always prints to stdout via
//! plain `println!`; diagnostic detail goes through `debug!`, which writes to
//! stderr only when `--verbose` is set. Keeping the two streams separate means
//! `restore -v 2>diag.log` captures diagnostics without polluting stdout.

use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Enable or disable verbose diagnostic output. Called once at startup from the
/// parsed `--verbose` flag.
pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

/// Whether `--verbose` was passed.
pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Print a diagnostic line to stderr, but only when `--verbose` is set.
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {{
        if $crate::log::is_verbose() {
            eprintln!($($arg)*);
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    // The only test touching the global flag, so it can own it without racing
    // other tests running in parallel.
    #[test]
    fn toggle_round_trips() {
        set_verbose(true);
        assert!(is_verbose());
        set_verbose(false);
        assert!(!is_verbose());
    }
}
