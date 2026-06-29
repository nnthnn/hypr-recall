use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::config::Config;
use crate::hyprland::{self, EventStream};
use crate::lock::LockGuard;
use crate::session::Session;

const SESSION_RESTORE_APPS: &[&str] = &[
    "firefox",
    "org.mozilla.firefox",
    "zed",
    "zed.zed.Zed",
    "dev.zed.Zed",
    "zeditor",
    "chromium",
    "google-chrome",
    "brave-browser",
    "discord",
];

fn spawn_overlay() -> Option<tokio::process::Child> {
    let mut path = std::env::current_exe().ok()?;
    path.set_file_name("hypr-recall-overlay");
    if !path.exists() {
        return None;
    }
    tokio::process::Command::new(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

pub async fn run(path: &Path, extra_restore_apps: &[String], cfg: &Config) -> Result<()> {
    if !path.exists() {
        eprintln!(
            "hypr-recall: no session file at {} — run 'hypr-recall save' first",
            path.display()
        );
        return Ok(());
    }

    let session = Session::load(path)?;
    let mut overlay = spawn_overlay();

    let lock_path = path.with_file_name("restore.lock");
    let _lock = LockGuard::acquire(lock_path)?;

    // Snapshot pre-existing window counts by class (before we launch anything)
    let pre_existing: HashMap<String, usize> = {
        let clients = hyprland::get_clients()?;
        let mut map: HashMap<String, usize> = HashMap::new();
        for c in &clients {
            *map.entry(c.initial_class.clone()).or_default() += 1;
        }
        map
    };

    // Subscribe to openwindow events before launching anything (avoids race)
    let rx = hyprland::subscribe_events().await?;
    let mut events = EventStream::new(rx);

    for ws_entry in &session.workspaces {
        let ws_id = ws_entry.workspace;
        println!(
            "hypr-recall: restoring workspace {ws_id} ({} windows)",
            ws_entry.windows.len()
        );

        hyprland::focus_workspace(ws_id)?;
        sleep(Duration::from_millis(200)).await;

        // Deduplicate within this workspace (same class processed once)
        let mut processed: HashSet<String> = HashSet::new();

        for window in &ws_entry.windows {
            let class = &window.class;

            if processed.contains(class) {
                continue;
            }
            processed.insert(class.clone());

            // Count saved windows of this class on this workspace
            let saved_count = ws_entry
                .windows
                .iter()
                .filter(|w| &w.class == class)
                .count();

            let pre = pre_existing.get(class).copied().unwrap_or(0);
            let needed = saved_count.saturating_sub(pre);

            if needed == 0 {
                println!("  {class}: skipped (pre-existing covers all {saved_count})");
                continue;
            }

            let before_total = hyprland::get_clients()?
                .into_iter()
                .filter(|c| &c.initial_class == class)
                .count();

            let exe = window.exe.trim_end_matches(" (deleted)");
            let target_total = before_total + needed;

            println!(
                "  {class}: saved={saved_count} pre={pre} needed={needed} before={before_total}"
            );

            let launch_args = cfg.launch_args(class, window.launch_args.as_ref());

            if cfg.is_session_restore_app(class, SESSION_RESTORE_APPS, extra_restore_apps) {
                // Launch once; the app restores all its windows itself
                let mut child = tokio::process::Command::new(exe)
                    .args(launch_args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| anyhow::anyhow!("failed to spawn {exe}: {e}"))?;

                let deadline = Instant::now() + Duration::from_secs(20);
                let got = events
                    .wait_for_count(class, needed, deadline, Some(&mut child))
                    .await;
                println!(
                    "  {class}: {got}/{needed} windows appeared (total: {})",
                    before_total + got
                );
            } else {
                // Launch one at a time and wait for each window
                for launch_n in 1..=needed {
                    let current = hyprland::get_clients()?
                        .into_iter()
                        .filter(|c| &c.initial_class == class)
                        .count();

                    if current >= target_total {
                        println!("  {class} launch {launch_n}/{needed}: already at {current}");
                        continue;
                    }

                    let mut child = tokio::process::Command::new(exe)
                        .args(launch_args)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .map_err(|e| anyhow::anyhow!("failed to spawn {exe}: {e}"))?;

                    let deadline = Instant::now() + Duration::from_secs(20);
                    let got = events
                        .wait_for_count(class, 1, deadline, Some(&mut child))
                        .await;
                    if got == 0 {
                        eprintln!("  TIMEOUT waiting for {class} (launch {launch_n}/{needed})");
                    }
                }
            }

            sleep(Duration::from_millis(300)).await;
        }

        // All windows for this workspace are open — reorder columns then apply widths
        reorder_columns(ws_id, ws_entry).await?;
    }

    fix_stray_windows(&session, cfg.settle_delay_secs).await?;

    if let Some(ref mut child) = overlay {
        let _ = child.kill().await;
    }

    hyprland::focus_workspace(session.active_workspace)?;
    println!("hypr-recall: restore complete");
    Ok(())
}

pub fn run_dry(path: &Path, extra_restore_apps: &[String], cfg: &Config) -> Result<()> {
    if !path.exists() {
        eprintln!(
            "hypr-recall: no session file at {} — run 'hypr-recall save' first",
            path.display()
        );
        return Ok(());
    }

    let session = Session::load(path)?;

    let pre_existing: HashMap<String, usize> = {
        let clients = hyprland::get_clients()?;
        let mut map: HashMap<String, usize> = HashMap::new();
        for c in &clients {
            *map.entry(c.initial_class.clone()).or_default() += 1;
        }
        map
    };

    println!("hypr-recall: dry run — no changes will be made\n");

    for ws_entry in &session.workspaces {
        let ws_id = ws_entry.workspace;
        let active = if ws_id == session.active_workspace {
            "  ← active"
        } else {
            ""
        };
        println!(
            "  workspace {} ({} window{}){active}",
            ws_id,
            ws_entry.windows.len(),
            if ws_entry.windows.len() == 1 { "" } else { "s" },
        );

        let mut processed: HashSet<String> = HashSet::new();

        for window in &ws_entry.windows {
            let class = &window.class;
            if processed.contains(class) {
                continue;
            }
            processed.insert(class.clone());

            let saved_count = ws_entry
                .windows
                .iter()
                .filter(|w| &w.class == class)
                .count();
            let pre = pre_existing.get(class).copied().unwrap_or(0);
            let needed = saved_count.saturating_sub(pre);

            let launch_args = cfg.launch_args(class, window.launch_args.as_ref());
            let args_suffix = if launch_args.is_empty() {
                String::new()
            } else {
                format!(" [args: {}]", launch_args.join(" "))
            };

            if needed == 0 {
                println!("    {class:<40} → skip ({pre} already open)");
            } else if cfg.is_session_restore_app(class, SESSION_RESTORE_APPS, extra_restore_apps) {
                println!(
                    "    {class:<40} → launch 1  [session-restore, waits for {needed} window{}]{args_suffix}",
                    if needed == 1 { "" } else { "s" }
                );
            } else {
                println!("    {class:<40} → launch {needed}{args_suffix}");
            }
        }
        println!();
    }

    Ok(())
}

pub struct LiveWindow {
    pub address: String,
    pub class: String,
}

/// Computes the swap operations needed to bring `live` into the order specified by `saved`.
///
/// Uses insertion sort: for each position, find the expected class and bubble it left.
/// Mutates `live` in place to simulate the swaps.
/// Returns `(address_to_focus, steps)` pairs ready to dispatch.
pub fn plan_column_swaps(saved: &[&str], live: &mut Vec<LiveWindow>) -> Vec<(String, usize)> {
    let n = saved.len().min(live.len());
    let mut ops = Vec::new();

    for (i, &expected) in saved.iter().enumerate().take(n.saturating_sub(1)) {
        if live[i].class == expected {
            continue;
        }

        let target_pos = live
            .iter()
            .enumerate()
            .skip(i + 1)
            .find(|(_, w)| w.class == expected)
            .map(|(j, _)| j);

        let Some(target_pos) = target_pos else {
            eprintln!("  reorder: no {expected} found after position {i}, skipping");
            continue;
        };

        let steps = target_pos - i;
        let addr = live[target_pos].address.clone();
        println!("  reorder: bubble {expected} from col {target_pos} → {i} ({steps} swap(s))");
        ops.push((addr, steps));

        let item = live.remove(target_pos);
        live.insert(i, item);
    }

    ops
}

/// After all workspaces are restored, some apps (e.g. Discord) open late windows
/// that land on the wrong workspace because focus has already moved on. Walk every
/// live client: if its class belongs to a workspace in the session and it ended up
/// somewhere else, silently move it to the expected workspace.
async fn fix_stray_windows(session: &crate::session::Session, settle_secs: u64) -> Result<()> {
    // Wait for late-opening windows (e.g. Discord Friends sidebar) to appear
    // before we sweep. Without this, the sweep runs before Discord finishes.
    sleep(Duration::from_secs(settle_secs)).await;

    // Build class → [expected workspace ids] from the saved session.
    // A class can appear on multiple workspaces (e.g. ghostty on ws2); each
    // unique workspace is recorded once, in session order.
    let mut class_to_ws: HashMap<String, Vec<i32>> = HashMap::new();
    for ws_entry in &session.workspaces {
        for win in &ws_entry.windows {
            let workspaces = class_to_ws.entry(win.class.clone()).or_default();
            if !workspaces.contains(&ws_entry.workspace) {
                workspaces.push(ws_entry.workspace);
            }
        }
    }

    let clients = hyprland::get_clients()?;
    let mut moved = 0usize;

    for client in &clients {
        let Some(valid_workspaces) = class_to_ws.get(&client.initial_class) else {
            continue;
        };
        if valid_workspaces.contains(&client.workspace_id) {
            continue;
        }
        // Window is on a workspace it doesn't belong to — move it silently.
        let target = valid_workspaces[0];
        println!(
            "  fix: {} strayed to ws{} → moving to ws{target}",
            client.initial_class, client.workspace_id
        );
        hyprland::move_to_workspace_silent(&client.address, target)?;
        sleep(Duration::from_millis(100)).await;
        moved += 1;
    }

    if moved > 0 {
        println!("hypr-recall: moved {moved} stray window(s) to correct workspace(s)");
    }

    Ok(())
}

async fn reorder_columns(ws_id: i32, ws_entry: &crate::session::WorkspaceEntry) -> Result<()> {
    sleep(Duration::from_millis(200)).await;

    let saved_classes: Vec<&str> = ws_entry.windows.iter().map(|w| w.class.as_str()).collect();

    let clients = hyprland::get_workspace_clients_sorted(ws_id)?;
    let mut live: Vec<LiveWindow> = clients
        .iter()
        .map(|c| LiveWindow {
            address: c.address.clone(),
            class: c.initial_class.clone(),
        })
        .collect();

    let ops = plan_column_swaps(&saved_classes, &mut live);

    for (addr, steps) in ops {
        hyprland::focus_window(&addr)?;
        for _ in 0..steps {
            hyprland::swapcol_left()?;
            sleep(Duration::from_millis(150)).await;
        }
    }

    // Apply col_width ratios in final sorted order
    sleep(Duration::from_millis(200)).await;
    let live_final = hyprland::get_workspace_clients_sorted(ws_id)?;

    for (i, window) in ws_entry.windows.iter().enumerate() {
        let Some(live_win) = live_final.get(i) else {
            continue;
        };
        hyprland::focus_window(&live_win.address)?;
        sleep(Duration::from_millis(200)).await;
        hyprland::colresize(window.col_width)?;
        sleep(Duration::from_millis(200)).await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lw(address: &str, class: &str) -> LiveWindow {
        LiveWindow {
            address: address.into(),
            class: class.into(),
        }
    }

    fn classes(live: &[LiveWindow]) -> Vec<&str> {
        live.iter().map(|w| w.class.as_str()).collect()
    }

    #[test]
    fn already_sorted_produces_no_ops() {
        let saved = ["a", "b", "c"];
        let mut live = vec![lw("0x1", "a"), lw("0x2", "b"), lw("0x3", "c")];
        let ops = plan_column_swaps(&saved, &mut live);
        assert!(ops.is_empty());
        assert_eq!(classes(&live), saved);
    }

    #[test]
    fn swap_two_adjacent() {
        let saved = ["a", "b"];
        let mut live = vec![lw("0x1", "b"), lw("0x2", "a")];
        let ops = plan_column_swaps(&saved, &mut live);
        assert_eq!(ops, vec![("0x2".to_owned(), 1)]);
        assert_eq!(classes(&live), saved);
    }

    #[test]
    fn bubble_from_end() {
        let saved = ["c", "a", "b"];
        let mut live = vec![lw("0x1", "a"), lw("0x2", "b"), lw("0x3", "c")];
        let ops = plan_column_swaps(&saved, &mut live);
        assert_eq!(ops, vec![("0x3".to_owned(), 2)]);
        assert_eq!(classes(&live), saved);
    }

    #[test]
    fn full_reverse() {
        let saved = ["c", "b", "a"];
        let mut live = vec![lw("0x1", "a"), lw("0x2", "b"), lw("0x3", "c")];
        plan_column_swaps(&saved, &mut live);
        assert_eq!(classes(&live), saved);
    }

    #[test]
    fn single_window_no_ops() {
        let saved = ["a"];
        let mut live = vec![lw("0x1", "a")];
        let ops = plan_column_swaps(&saved, &mut live);
        assert!(ops.is_empty());
    }

    #[test]
    fn missing_class_skipped_no_panic() {
        let saved = ["x", "a", "b"];
        let mut live = vec![lw("0x1", "a"), lw("0x2", "b")];
        let ops = plan_column_swaps(&saved, &mut live);
        // "x" not in live — should produce no op for it and not panic
        assert!(ops.is_empty());
    }

    #[test]
    fn duplicate_classes_already_sorted() {
        let saved = ["firefox", "ghostty", "ghostty"];
        let mut live = vec![
            lw("0x1", "firefox"),
            lw("0x2", "ghostty"),
            lw("0x3", "ghostty"),
        ];
        let ops = plan_column_swaps(&saved, &mut live);
        assert!(ops.is_empty());
        assert_eq!(classes(&live), saved);
    }

    #[test]
    fn duplicate_classes_need_reorder() {
        let saved = ["ghostty", "firefox", "ghostty"];
        let mut live = vec![
            lw("0x1", "firefox"),
            lw("0x2", "ghostty"),
            lw("0x3", "ghostty"),
        ];
        plan_column_swaps(&saved, &mut live);
        assert_eq!(classes(&live), saved);
    }
}
