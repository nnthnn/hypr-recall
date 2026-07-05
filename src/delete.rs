use anyhow::{Context, Result};
use std::path::Path;

pub fn run(path: &Path) -> Result<()> {
    if !path.exists() {
        println!(
            "{}: no session file at {}",
            crate::color::hr(),
            path.display()
        );
        return Ok(());
    }

    std::fs::remove_file(path).with_context(|| format!("failed to delete {}", path.display()))?;

    println!("{}: deleted {}", crate::color::hr(), path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hypr-recall-delete-test-{name}.json"))
    }

    #[test]
    fn deletes_existing_file() {
        let path = tmp("existing");
        std::fs::write(&path, "{}").unwrap();
        assert!(run(&path).is_ok());
        assert!(!path.exists());
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let path = tmp("missing");
        let _ = std::fs::remove_file(&path);
        assert!(run(&path).is_ok());
    }
}
