//! Minimal verbosity control. High-level progress goes through `progress!`,
//! which writes to stdout unless `--quiet` is set; diagnostic detail goes
//! through `debug!`, which writes to stderr only when `--verbose` is set.
//! Warnings and errors are always printed via plain `eprintln!` at their call
//! sites, regardless of either flag. Keeping the streams separate means
//! `restore -v 2>diag.log` captures diagnostics without polluting stdout, and
//! `restore -q` (e.g. from an autostart hook) silences progress but not errors.

use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);
static QUIET: AtomicBool = AtomicBool::new(false);

/// Enable or disable verbose diagnostic output. Called once at startup from the
/// parsed `--verbose` flag.
pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

/// Whether `--verbose` was passed.
pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Enable or disable quiet mode. Called once at startup from the parsed
/// `--quiet` flag.
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

/// Whether `--quiet` was passed.
pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
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

/// Print a high-level progress line to stdout, unless `--quiet` is set.
#[macro_export]
macro_rules! progress {
    ($($arg:tt)*) => {{
        if !$crate::log::is_quiet() {
            println!($($arg)*);
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    // The only tests touching the global flags, so they can own them without
    // racing other tests running in parallel.
    #[test]
    fn verbose_toggle_round_trips() {
        set_verbose(true);
        assert!(is_verbose());
        set_verbose(false);
        assert!(!is_verbose());
    }

    #[test]
    fn quiet_toggle_round_trips() {
        set_quiet(true);
        assert!(is_quiet());
        set_quiet(false);
        assert!(!is_quiet());
    }
}
