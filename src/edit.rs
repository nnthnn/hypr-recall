use anyhow::{Context, Result};
use std::path::Path;

pub fn run(path: &Path) -> Result<()> {
    if !path.exists() {
        eprintln!(
            "hypr-recall: no session file at {} — run 'hypr-recall save' first",
            path.display()
        );
        return Ok(());
    }

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .context("no editor found — set $VISUAL or $EDITOR")?;

    std::process::Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("failed to launch editor '{editor}'"))?;

    Ok(())
}
