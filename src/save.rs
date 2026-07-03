use anyhow::{Context, Result};
use std::path::Path;

use crate::hyprland;
use crate::session::{Session, WindowEntry, WorkspaceEntry};

/// Scan live Hyprland clients into `WorkspaceEntry` groups, sorted by
/// workspace then left-to-right column order. When `only_workspace` is
/// `Some`, only that workspace's windows are captured.
fn capture_workspaces(only_workspace: Option<i32>) -> Result<Vec<WorkspaceEntry>> {
    let monitor_widths = hyprland::get_monitor_widths()?;
    let clients = hyprland::get_clients()?;

    let mut rows: Vec<(i32, i32, WindowEntry)> = Vec::new();

    for client in &clients {
        if !client.mapped || client.floating || client.workspace_id <= 0 {
            continue;
        }
        if let Some(only) = only_workspace {
            if client.workspace_id != only {
                continue;
            }
        }

        let exe = match std::fs::read_link(format!("/proc/{}/exe", client.pid)) {
            Ok(p) => {
                let s = p.to_string_lossy().into_owned();
                // Strip " (deleted)" suffix left by package updates
                s.trim_end_matches(" (deleted)").to_owned()
            }
            Err(_) => continue,
        };

        let monitor_width = monitor_widths.get(&client.monitor).copied().unwrap_or(1920);

        let col_width =
            (f64::from(client.width) / f64::from(monitor_width) * 1000.0).round() / 1000.0;

        rows.push((
            client.workspace_id,
            client.x,
            WindowEntry {
                class: client.initial_class.clone(),
                exe,
                launch_args: None,
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

    Ok(workspaces)
}

pub fn run(path: &Path, only_workspace: Option<i32>) -> Result<()> {
    // Skip if a restore is in progress
    let lock_path = path.with_file_name("restore.lock");
    if lock_path.exists() {
        eprintln!(
            "{}: restore in progress, skipping save",
            crate::color::hr_err()
        );
        return Ok(());
    }

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session")
        .to_owned();

    match only_workspace {
        None => run_full(path, &name),
        Some(id) => run_scoped(path, &name, id),
    }
}

fn run_full(path: &Path, name: &str) -> Result<()> {
    let active_workspace = hyprland::get_active_workspace_id()?;
    let workspaces = capture_workspaces(None)?;
    let total_windows: usize = workspaces.iter().map(|ws| ws.windows.len()).sum();

    let session = Session {
        version: crate::session::SESSION_VERSION,
        active_workspace,
        workspaces,
    };

    session.save_to(path)?;
    println!(
        "{}: saved '{name}' — {} windows across {} workspaces",
        crate::color::hr(),
        total_windows,
        session.workspaces.len(),
    );
    Ok(())
}

fn run_scoped(path: &Path, name: &str, id: i32) -> Result<()> {
    let captured = capture_workspaces(Some(id))?.pop();

    let mut session = if path.exists() {
        Session::load(path).with_context(|| {
            format!("failed to load existing session '{name}' — refusing to overwrite it")
        })?
    } else {
        Session {
            version: crate::session::SESSION_VERSION,
            active_workspace: id,
            workspaces: Vec::new(),
        }
    };

    let had_entry = session.workspaces.iter().any(|w| w.workspace == id);

    match captured {
        Some(entry) => {
            let count = entry.windows.len();
            session.merge_workspace(id, Some(entry));
            session.save_to(path)?;
            println!(
                "{}: saved workspace {id} to '{name}' — {count} windows (workspace {id} only)",
                crate::color::hr(),
            );
        }
        None if had_entry => {
            session.merge_workspace(id, None);
            session.save_to(path)?;
            println!(
                "{}: workspace {id} has no windows, removed from '{name}'",
                crate::color::hr(),
            );
        }
        None => {
            println!(
                "{}: workspace {id} has no windows, nothing to save",
                crate::color::hr(),
            );
        }
    }

    Ok(())
}
