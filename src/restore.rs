use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::time::sleep;

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
];

fn is_restore_app(class: &str) -> bool {
    SESSION_RESTORE_APPS.contains(&class)
}

pub async fn run(path: &Path) -> Result<()> {
    if !path.exists() {
        eprintln!(
            "hypr-recall: no session file at {} — run 'hypr-recall save' first",
            path.display()
        );
        return Ok(());
    }

    let session = Session::load(path)?;

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
    let rx = hyprland::subscribe_openwindow().await?;
    let mut events = EventStream::new(rx);

    for ws_entry in &session.workspaces {
        let ws_id = ws_entry.workspace;
        println!("hypr-recall: restoring workspace {ws_id} ({} windows)", ws_entry.windows.len());

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

            println!("  {class}: saved={saved_count} pre={pre} needed={needed} before={before_total}");

            if is_restore_app(class) {
                // Launch once; the app restores all its windows itself
                let mut child = tokio::process::Command::new(exe)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| anyhow::anyhow!("failed to spawn {exe}: {e}"))?;

                let deadline = Instant::now() + Duration::from_secs(20);
                let got = events
                    .wait_for_count(class, needed, deadline, Some(&mut child))
                    .await;
                println!("  {class}: {got}/{needed} windows appeared (total: {})", before_total + got);
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

    hyprland::focus_workspace(session.active_workspace)?;
    println!("hypr-recall: restore complete");
    Ok(())
}

async fn reorder_columns(ws_id: i32, ws_entry: &crate::session::WorkspaceEntry) -> Result<()> {
    sleep(Duration::from_millis(200)).await;

    let saved_classes: Vec<&str> = ws_entry.windows.iter().map(|w| w.class.as_str()).collect();
    let n = saved_classes.len();

    // Insertion sort: for each position i, find the correct window and bubble it left
    for i in 0..n.saturating_sub(1) {
        let expected = saved_classes[i];

        // Re-query live positions each iteration (positions shift after swapcol)
        let live = hyprland::get_workspace_clients_sorted(ws_id)?;

        let actual = live.get(i).map(|c| c.initial_class.as_str()).unwrap_or("");
        if actual == expected {
            continue;
        }

        // Find where expected class is (search from i+1 onward)
        let target_pos = live
            .iter()
            .enumerate()
            .skip(i + 1)
            .find(|(_, c)| c.initial_class == expected)
            .map(|(j, _)| j);

        let Some(target_pos) = target_pos else {
            eprintln!("  reorder: no {expected} found after position {i}, skipping");
            continue;
        };

        let steps = target_pos - i;
        let target_addr = &live[target_pos].address;
        println!("  reorder: bubble {expected} from col {target_pos} → {i} ({steps} swap(s))");

        hyprland::focus_window(target_addr)?;
        for _ in 0..steps {
            hyprland::swapcol_left()?;
            sleep(Duration::from_millis(150)).await;
        }
    }

    // Apply col_width ratios in final sorted order
    sleep(Duration::from_millis(200)).await;
    let live = hyprland::get_workspace_clients_sorted(ws_id)?;

    for (i, window) in ws_entry.windows.iter().enumerate() {
        let Some(live_win) = live.get(i) else {
            continue;
        };
        hyprland::focus_window(&live_win.address)?;
        sleep(Duration::from_millis(200)).await;
        hyprland::colresize(window.col_width)?;
        sleep(Duration::from_millis(200)).await;
    }

    Ok(())
}
