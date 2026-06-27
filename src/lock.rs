use anyhow::Result;
use std::path::PathBuf;

pub struct LockGuard(PathBuf);

impl LockGuard {
    pub fn acquire(path: PathBuf) -> Result<Self> {
        if path.exists() {
            anyhow::bail!(
                "restore already running (remove {} to override)",
                path.display()
            );
        }
        std::fs::write(&path, std::process::id().to_string())?;
        Ok(Self(path))
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
