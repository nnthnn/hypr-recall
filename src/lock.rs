use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct LockGuard(PathBuf);

impl LockGuard {
    pub fn acquire(path: PathBuf) -> Result<Self> {
        if path.exists() && holder_is_alive(&path) {
            anyhow::bail!(
                "restore already running (remove {} to override)",
                path.display()
            );
        }
        // Either no lock, or a stale one left by a crashed/killed restore —
        // reclaim it by overwriting with our PID.
        std::fs::write(&path, std::process::id().to_string())?;
        Ok(Self(path))
    }
}

/// Whether the process that wrote `path` is still running.
///
/// Returns `true` only when the file holds a PID that names a live process.
/// An unreadable file, an unparseable PID, or a dead process all count as
/// stale (`false`) so a lock left behind by a crashed restore is reclaimed
/// rather than wedging the tool forever.
fn holder_is_alive(path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(pid) = contents.trim().parse::<u32>() else {
        return false;
    };
    Path::new(&format!("/proc/{pid}")).exists()
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
}
