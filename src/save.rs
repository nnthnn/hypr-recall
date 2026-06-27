use anyhow::Result;
use std::path::Path;

use crate::hyprland;
use crate::session::{Session, WindowEntry, WorkspaceEntry};

pub fn run(path: &Path) -> Result<()> {
    // Skip if a restore is in progress
    let lock_path = path.with_file_name("restore.lock");
    if lock_path.exists() {
        eprintln!("hypr-recall: restore in progress, skipping save");
        return Ok(());
    }

    let active_workspace = hyprland::get_active_workspace_id()?;
    let monitor_widths = hyprland::get_monitor_widths()?;
    let clients = hyprland::get_clients()?;

    // Collect (workspace_id, x, entry) for visible tiled windows
    let mut rows: Vec<(i32, i32, WindowEntry)> = Vec::new();

    for client in &clients {
        if !client.mapped || client.floating || client.workspace_id <= 0 {
            continue;
        }

        let exe = match std::fs::read_link(format!("/proc/{}/exe", client.pid)) {
            Ok(p) => {
                let s = p.to_string_lossy().into_owned();
                // Strip " (deleted)" suffix left by package updates
                s.trim_end_matches(" (deleted)").to_string()
            }
            Err(_) => continue,
        };

        let monitor_width = monitor_widths
            .get(&client.monitor)
            .copied()
            .unwrap_or(1920);

        let col_width = (client.width as f64 / monitor_width as f64 * 1000.0).round() / 1000.0;

        rows.push((
            client.workspace_id,
            client.x,
            WindowEntry {
                class: client.initial_class.clone(),
                exe,
                col_width,
            },
        ));
    }

    // Sort by workspace then x-position
    rows.sort_by_key(|(ws, x, _)| (*ws, *x));

    // Group into workspaces
    let mut workspaces: Vec<WorkspaceEntry> = Vec::new();
    let mut current_ws: Option<WorkspaceEntry> = None;

    for (ws_id, _x, entry) in rows {
        match current_ws.as_mut() {
            Some(ws) if ws.workspace == ws_id => ws.windows.push(entry),
            _ => {
                if let Some(ws) = current_ws.take() {
                    workspaces.push(ws);
                }
                current_ws = Some(WorkspaceEntry {
                    workspace: ws_id,
                    windows: vec![entry],
                });
            }
        }
    }
    if let Some(ws) = current_ws {
        workspaces.push(ws);
    }

    let total_windows: usize = workspaces.iter().map(|ws| ws.windows.len()).sum();

    let session = Session {
        active_workspace,
        workspaces,
    };

    session.save_to(path)?;
    println!(
        "hypr-recall: saved {} windows across {} workspaces → {}",
        total_windows,
        session.workspaces.len(),
        path.display()
    );
    Ok(())
}
