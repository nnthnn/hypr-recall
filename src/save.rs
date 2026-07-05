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

    Ok(group_into_workspaces(rows))
}

/// Sort `(workspace_id, x_position, entry)` rows and group them into
/// `WorkspaceEntry`s, preserving left-to-right window order within each.
fn group_into_workspaces(mut rows: Vec<(i32, i32, WindowEntry)>) -> Vec<WorkspaceEntry> {
    rows.sort_by_key(|(ws, x, _)| (*ws, *x));

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

    workspaces
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
    crate::progress!(
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
    let count = captured.as_ref().map(|entry| entry.windows.len());
    let outcome = scoped_outcome(had_entry, count);
    session.merge_workspace(id, captured);

    match outcome {
        ScopedOutcome::Saved(count) => {
            session.save_to(path)?;
            crate::progress!(
                "{}: saved workspace {id} to '{name}' — {count} windows (workspace {id} only)",
                crate::color::hr(),
            );
        }
        ScopedOutcome::Removed => {
            session.save_to(path)?;
            crate::progress!(
                "{}: workspace {id} has no windows, removed from '{name}'",
                crate::color::hr(),
            );
        }
        ScopedOutcome::NoOp => {
            crate::progress!(
                "{}: workspace {id} has no windows, nothing to save",
                crate::color::hr(),
            );
        }
    }

    Ok(())
}

#[derive(Debug, PartialEq)]
enum ScopedOutcome {
    /// Workspace had windows — write them to the session file.
    Saved(usize),
    /// Workspace had no windows but a prior entry existed — write the removal.
    Removed,
    /// Workspace had no windows and no prior entry — nothing to write.
    NoOp,
}

fn scoped_outcome(had_entry: bool, count: Option<usize>) -> ScopedOutcome {
    match (count, had_entry) {
        (Some(count), _) => ScopedOutcome::Saved(count),
        (None, true) => ScopedOutcome::Removed,
        (None, false) => ScopedOutcome::NoOp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(class: &str) -> WindowEntry {
        WindowEntry {
            class: class.to_owned(),
            exe: format!("/usr/bin/{class}"),
            launch_args: None,
            col_width: 0.5,
        }
    }

    #[test]
    fn group_into_workspaces_orders_by_workspace_then_x() {
        let rows = vec![
            (2, 100, window("b")),
            (1, 200, window("a2")),
            (1, 0, window("a1")),
            (2, 0, window("a")),
        ];

        let workspaces = group_into_workspaces(rows);

        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces[0].workspace, 1);
        assert_eq!(
            workspaces[0]
                .windows
                .iter()
                .map(|w| w.class.as_str())
                .collect::<Vec<_>>(),
            vec!["a1", "a2"]
        );
        assert_eq!(workspaces[1].workspace, 2);
        assert_eq!(
            workspaces[1]
                .windows
                .iter()
                .map(|w| w.class.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn group_into_workspaces_empty_input_is_empty() {
        assert_eq!(group_into_workspaces(Vec::new()), Vec::new());
    }

    #[test]
    fn scoped_outcome_saved_when_windows_captured() {
        assert_eq!(scoped_outcome(false, Some(3)), ScopedOutcome::Saved(3));
        assert_eq!(scoped_outcome(true, Some(0)), ScopedOutcome::Saved(0));
    }

    #[test]
    fn scoped_outcome_removed_when_empty_but_previously_present() {
        assert_eq!(scoped_outcome(true, None), ScopedOutcome::Removed);
    }

    #[test]
    fn scoped_outcome_noop_when_empty_and_never_present() {
        assert_eq!(scoped_outcome(false, None), ScopedOutcome::NoOp);
    }
}
