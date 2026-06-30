use anyhow::Result;
use std::path::Path;
use std::time::SystemTime;

use crate::session::Session;

pub fn run(dir: &Path) -> Result<()> {
    if !dir.exists() {
        println!("{}: no sessions saved yet", crate::color::hr());
        println!("  run 'hypr-recall save [name]' to create one");
        return Ok(());
    }

    let mut entries: Vec<(String, std::path::PathBuf, SystemTime)> = std::fs::read_dir(dir)?
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_stem()?.to_str()?.to_owned();
            if path.extension()?.to_str()? != "json" {
                return None;
            }
            // exclude the lock file sentinel (restore.lock is not JSON, but be safe)
            if name == "restore" {
                return None;
            }
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((name, path, modified))
        })
        .collect();

    if entries.is_empty() {
        println!("{}: no sessions saved yet", crate::color::hr());
        println!("  run 'hypr-recall save [name]' to create one");
        return Ok(());
    }

    // Sort newest first
    entries.sort_by_key(|e| std::cmp::Reverse(e.2));

    println!("{}: saved sessions\n", crate::color::hr());

    for (name, path, modified) in &entries {
        let age = format_age(*modified);
        let summary = session_summary(path);
        println!("  {name:<20} {age:<16} {summary}");
    }

    Ok(())
}

fn session_summary(path: &Path) -> String {
    let Ok(session) = Session::load(path) else {
        return "(unreadable)".to_owned();
    };
    let total: usize = session.workspaces.iter().map(|ws| ws.windows.len()).sum();
    format!(
        "{} workspace{}, {} window{}",
        session.workspaces.len(),
        if session.workspaces.len() == 1 {
            ""
        } else {
            "s"
        },
        total,
        if total == 1 { "" } else { "s" },
    )
}

fn format_age(modified: SystemTime) -> String {
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        return "just now".to_owned();
    };
    let secs = age.as_secs();
    if secs < 60 {
        "just now".to_owned()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86400 {
        format!("{} hr ago", secs / 3600)
    } else {
        format!("{} days ago", secs / 86400)
    }
}
