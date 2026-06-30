use anyhow::Result;
use std::path::Path;
use std::time::SystemTime;

use crate::session::Session;

pub fn run(path: &Path) -> Result<()> {
    if !path.exists() {
        println!(
            "{}: no session file at {}",
            crate::color::hr(),
            path.display()
        );
        println!("  run 'hypr-recall save' to create one");
        return Ok(());
    }

    let session = Session::load(path)?;
    let age = file_age(path);

    let total_windows: usize = session.workspaces.iter().map(|ws| ws.windows.len()).sum();

    println!(
        "{}: {} ({} workspace{}, {} window{})",
        crate::color::hr(),
        age,
        session.workspaces.len(),
        if session.workspaces.len() == 1 {
            ""
        } else {
            "s"
        },
        total_windows,
        if total_windows == 1 { "" } else { "s" },
    );

    for ws in &session.workspaces {
        let active = if ws.workspace == session.active_workspace {
            "  ← active"
        } else {
            ""
        };
        println!("\n  workspace {}{}", ws.workspace, active);
        for win in &ws.windows {
            println!("    {:<40} {:.0}%", win.class, win.col_width * 100.0);
        }
    }

    Ok(())
}

fn file_age(path: &Path) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return format!("session at {}", path.display());
    };
    let Ok(modified) = meta.modified() else {
        return format!("session at {}", path.display());
    };
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        return "session saved just now".to_owned();
    };

    let secs = age.as_secs();
    let age_str = if secs < 60 {
        "just now".to_owned()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86400 {
        format!("{} hr ago", secs / 3600)
    } else {
        format!("{} days ago", secs / 86400)
    };

    format!("session saved {age_str}")
}
