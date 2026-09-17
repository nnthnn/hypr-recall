use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct LockGuard(PathBuf);

impl LockGuard {
    /// Acquire the lock atomically, so two restores racing can't both win.
    ///
    /// The lock is published by hard-linking a fully-written temp file into
    /// place: `hard_link` fails with `AlreadyExists` instead of replacing an
    /// existing file, and the PID content is visible the instant the link
    /// appears. That closes the read-then-write TOCTOU window a plain
    /// `is_held`-then-`write` sequence has, where both processes could observe
    /// no lock and then both write their own PID to it.
    ///
    /// A lock left behind by a crashed process is still reclaimed: when the
    /// published lock names a dead PID (or is empty/garbage) it's removed and
    /// the link retried.
    pub fn acquire(path: PathBuf) -> Result<Self> {
        // Same directory as the lock, so `hard_link` can't cross filesystems.
        let tmp = path.with_extension(format!("lock.tmp.{}", std::process::id()));
        std::fs::write(&tmp, std::process::id().to_string())?;

        // Bounded: each iteration acquires, bails on a live holder, or clears
        // one stale lock. A few retries covers a racing peer.
        for _ in 0..5 {
            match std::fs::hard_link(&tmp, &path) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&tmp);
                    return Ok(Self(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::is_held(&path) {
                        let _ = std::fs::remove_file(&tmp);
                        anyhow::bail!(
                            "restore already running (remove {} to override)",
                            path.display()
                        );
                    }
                    // Stale lock (dead PID, empty, or unparseable) — clear it.
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e.into());
                }
            }
        }

        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("could not acquire lock at {}", path.display())
    }

    /// Whether the process that wrote `path` is still running.
    ///
    /// Returns `true` only when the file holds a PID that names a live
    /// process. A missing/unreadable file, an unparseable PID, or a dead
    /// process all count as not held (`false`) so a lock left behind by a
    /// crashed restore is reclaimed rather than wedging the tool forever.
    pub(crate) fn is_held(path: &Path) -> bool {
        let Ok(contents) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(pid) = contents.trim().parse::<u32>() else {
            return false;
        };
        Path::new(&format!("/proc/{pid}")).exists()
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hypr-recall-lock-test-{name}.lock"))
    }

    #[test]
    fn acquire_creates_and_drop_removes() {
        let path = tmp("create-remove");
        let _ = std::fs::remove_file(&path);
        {
            let _guard = LockGuard::acquire(path.clone()).unwrap();
            assert!(path.exists());
        }
        assert!(!path.exists(), "lock should be removed on drop");
    }

    #[test]
    fn live_holder_blocks_acquire() {
        // Our own PID is, by definition, alive.
        let path = tmp("live-holder");
        std::fs::write(&path, std::process::id().to_string()).unwrap();
        assert!(LockGuard::acquire(path.clone()).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn stale_pid_is_reclaimed() {
        // PID 0 never names a real process, so the lock is stale.
        let path = tmp("stale-pid");
        std::fs::write(&path, "0").unwrap();
        let guard = LockGuard::acquire(path.clone());
        assert!(guard.is_ok(), "stale lock should be reclaimed");
        drop(guard);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn garbage_lock_is_reclaimed() {
        let path = tmp("garbage");
        std::fs::write(&path, "not-a-pid").unwrap();
        assert!(LockGuard::acquire(path.clone()).is_ok());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn concurrent_acquire_yields_exactly_one_winner() {
        // A plain exists()-then-write would let several racers all see no lock
        // and all "acquire" it. Atomic publication must let exactly one win.
        let path = tmp("concurrent");
        let _ = std::fs::remove_file(&path);

        let results: Vec<Result<LockGuard>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| LockGuard::acquire(path.clone())))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        // The winner's guard lives in `results` until after the assertion, so
        // the other threads can't reclaim it by seeing a dead PID.
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "exactly one racing acquirer may hold the lock"
        );
        drop(results);
        std::fs::remove_file(&path).ok();
    }
}
