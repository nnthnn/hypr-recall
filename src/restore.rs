use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::config::Config;
use crate::hyprland::{self, EventStream};
use crate::lock::LockGuard;
use crate::session::{Session, WorkspaceEntry};

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

struct OverlayHandle {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
}

impl OverlayHandle {
    async fn send_progress(&mut self, current: usize, total: usize) {
        use tokio::io::AsyncWriteExt;
        let msg = format!("restoring workspace {current} / {total}\n");
        let _ = self.stdin.write_all(msg.as_bytes()).await;
    }

    async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

fn spawn_overlay() -> Option<OverlayHandle> {
    let mut path = std::env::current_exe().ok()?;
    path.set_file_name("hypr-recall-overlay");
    if !path.exists() {
        return None;
    }
    let mut child = tokio::process::Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Tear the overlay down if `run` returns early via `?` before the
        // explicit kill — otherwise a mid-restore error leaves the fullscreen
        // layer-shell window stuck on screen with no way to dismiss it.
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let stdin = child.stdin.take()?;
    Some(OverlayHandle { child, stdin })
}

/// One app class's launch plan for a single workspace. Shared by `run` (real
/// restore) and `run_dry` (preview) so both agree on what gets launched.
#[derive(Debug, PartialEq)]
pub struct ClassPlan {
    pub class: String,
    pub exe: String,
    pub launch_args: Vec<String>,
    pub saved_count: usize,
    pub pre: usize,
    pub needed: usize,
    pub session_restore: bool,
}

/// Build the per-class plan for a workspace, in saved column order, deduplicated
/// by class (first occurrence wins for `exe`/`launch_args`). `pre_existing` is
/// the count of already-open windows per class, snapshotted once before any
/// workspace is restored.
fn plan_workspace(
    ws_entry: &crate::session::WorkspaceEntry,
    pre_existing: &HashMap<String, usize>,
    cfg: &Config,
    extra_restore_apps: &[String],
) -> Vec<ClassPlan> {
    let mut processed: HashSet<String> = HashSet::new();
    let mut plans = Vec::new();

    for window in &ws_entry.windows {
        let class = &window.class;
        if !processed.insert(class.clone()) {
            continue;
        }

        let saved_count = ws_entry
            .windows
            .iter()
            .filter(|w| &w.class == class)
            .count();
        let pre = pre_existing.get(class).copied().unwrap_or(0);

        plans.push(ClassPlan {
            class: class.clone(),
            exe: window.exe.trim_end_matches(" (deleted)").to_owned(),
            launch_args: cfg.launch_args(class, window.launch_args.as_ref()).to_vec(),
            saved_count,
            pre,
            needed: saved_count.saturating_sub(pre),
            session_restore: cfg.is_session_restore_app(
                class,
                SESSION_RESTORE_APPS,
                extra_restore_apps,
            ),
        });
    }

    plans
}

/// Select the workspaces to act on. With `only` set, restrict to that single
/// workspace; otherwise return all of them. Returns `None` if `only` was given
/// but no such workspace exists in the session.
fn select_workspaces(session: &Session, only: Option<i32>) -> Option<Vec<&WorkspaceEntry>> {
    match only {
        Some(id) => {
            let selected: Vec<&WorkspaceEntry> = session
                .workspaces
                .iter()
                .filter(|w| w.workspace == id)
                .collect();
            (!selected.is_empty()).then_some(selected)
        }
        None => Some(session.workspaces.iter().collect()),
    }
}

pub async fn run(
    path: &Path,
    extra_restore_apps: &[String],
    cfg: &Config,
    only_workspace: Option<i32>,
) -> Result<()> {
    if !path.exists() {
        eprintln!(
            "{}: no session file at {} — run 'hypr-recall save' first",
            crate::color::hr_err(),
            path.display()
        );
        return Ok(());
    }

    let session = Session::load(path)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session");

    let Some(workspaces) = select_workspaces(&session, only_workspace) else {
        eprintln!(
            "{}: workspace {} not found in session '{name}'",
            crate::color::hr_err(),
            only_workspace.unwrap_or_default()
        );
        return Ok(());
    };

    let scope = only_workspace.map_or(String::new(), |w| format!(" (workspace {w} only)"));
    println!("{}: restoring '{name}'{scope}", crate::color::hr());
    let mut overlay = if cfg.overlay { spawn_overlay() } else { None };

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

    let total_workspaces = workspaces.len();
    for (ws_idx, &ws_entry) in workspaces.iter().enumerate() {
        let ws_id = ws_entry.workspace;
        println!(
            "{}: restoring workspace {ws_id} ({} windows)",
            crate::color::hr(),
            ws_entry.windows.len()
        );
        if let Some(ref mut ov) = overlay {
            ov.send_progress(ws_idx + 1, total_workspaces).await;
        }

        hyprland::focus_workspace(ws_id)?;
        sleep(Duration::from_millis(200)).await;

        for plan in plan_workspace(ws_entry, &pre_existing, cfg, extra_restore_apps) {
            let class = &plan.class;
            let needed = plan.needed;

            if needed == 0 {
                crate::debug!(
                    "  {class}: skipped (pre-existing covers all {})",
                    plan.saved_count
                );
                continue;
            }

            let before_total = hyprland::get_clients()?
                .into_iter()
                .filter(|c| &c.initial_class == class)
                .count();

            let exe = plan.exe.as_str();
            let target_total = before_total + needed;

            crate::debug!(
                "  {class}: saved={} pre={} needed={needed} before={before_total}",
                plan.saved_count,
                plan.pre
            );

            if plan.session_restore {
                // Launch once; the app restores all its windows itself
                let mut child = tokio::process::Command::new(exe)
                    .args(&plan.launch_args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| anyhow::anyhow!("failed to spawn {exe}: {e}"))?;

                let deadline = Instant::now() + Duration::from_secs(20);
                let got = events
                    .wait_for_count(class, needed, deadline, Some(&mut child))
                    .await;
                crate::debug!(
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
                        crate::debug!("  {class} launch {launch_n}/{needed}: already at {current}");
                        continue;
                    }

                    let mut child = tokio::process::Command::new(exe)
                        .args(&plan.launch_args)
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

    fix_stray_windows(&workspaces, cfg.settle_delay_secs).await?;

    if let Some(ref mut ov) = overlay {
        ov.kill().await;
    }

    // For a single-workspace restore, end focused on that workspace rather than
    // jumping to the session's saved active workspace (which we didn't restore).
    hyprland::focus_workspace(only_workspace.unwrap_or(session.active_workspace))?;
    println!("{}: restore complete", crate::color::hr());
    Ok(())
}

pub fn run_dry(
    path: &Path,
    extra_restore_apps: &[String],
    cfg: &Config,
    only_workspace: Option<i32>,
) -> Result<()> {
    if !path.exists() {
        eprintln!(
            "{}: no session file at {} — run 'hypr-recall save' first",
            crate::color::hr_err(),
            path.display()
        );
        return Ok(());
    }

    let session = Session::load(path)?;

    let Some(workspaces) = select_workspaces(&session, only_workspace) else {
        eprintln!(
            "{}: workspace {} not found in session",
            crate::color::hr_err(),
            only_workspace.unwrap_or_default()
        );
        return Ok(());
    };

    let pre_existing: HashMap<String, usize> = {
        let clients = hyprland::get_clients()?;
        let mut map: HashMap<String, usize> = HashMap::new();
        for c in &clients {
            *map.entry(c.initial_class.clone()).or_default() += 1;
        }
        map
    };

    println!(
        "{}: dry run — no changes will be made\n",
        crate::color::hr()
    );

    for &ws_entry in &workspaces {
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

        for plan in plan_workspace(ws_entry, &pre_existing, cfg, extra_restore_apps) {
            let class = &plan.class;
            let needed = plan.needed;

            let args_suffix = if plan.launch_args.is_empty() {
                String::new()
            } else {
                format!(" [args: {}]", plan.launch_args.join(" "))
            };

            if needed == 0 {
                println!("    {class:<40} → skip ({} already open)", plan.pre);
            } else if plan.session_restore {
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
            crate::debug!("  reorder: no {expected} found after position {i}, skipping");
            continue;
        };

        let steps = target_pos - i;
        let addr = live[target_pos].address.clone();
        crate::debug!("  reorder: bubble {expected} from col {target_pos} → {i} ({steps} swap(s))");
        ops.push((addr, steps));

        let item = live.remove(target_pos);
        live.insert(i, item);
    }

    ops
}

/// Pairs each saved window with a live window of the same class, consuming live
/// windows left to right, and returns the `(address, col_width)` ops to apply.
///
/// Matching by class rather than by index keeps widths aligned with the right
/// columns even when a window failed to launch and `live` is shorter than
/// `saved`: a saved entry with no surviving match is simply skipped instead of
/// shifting every subsequent width onto the wrong window.
pub fn plan_width_assignments(saved: &[(&str, f64)], live: &[LiveWindow]) -> Vec<(String, f64)> {
    let mut consumed = vec![false; live.len()];
    let mut ops = Vec::new();

    for &(class, width) in saved {
        let Some(idx) = live
            .iter()
            .enumerate()
            .position(|(i, w)| !consumed[i] && w.class == class)
        else {
            continue;
        };
        consumed[idx] = true;
        ops.push((live[idx].address.clone(), width));
    }

    ops
}

/// After the restored workspaces are populated, some apps (e.g. Discord) open
/// late windows that land on the wrong workspace because focus has already moved
/// on. Walk every live client: if its class belongs to a restored workspace and
/// it ended up somewhere else, silently move it to the expected workspace.
///
/// `workspaces` is the set actually restored, so a single-workspace restore only
/// ever sweeps windows toward that one workspace and never disturbs others.
async fn fix_stray_windows(workspaces: &[&WorkspaceEntry], settle_secs: u64) -> Result<()> {
    // Wait for late-opening windows (e.g. Discord Friends sidebar) to appear
    // before we sweep. Without this, the sweep runs before Discord finishes.
    sleep(Duration::from_secs(settle_secs)).await;

    // Build class → [expected workspace ids] from the restored workspaces.
    // A class can appear on multiple workspaces (e.g. ghostty on ws2); each
    // unique workspace is recorded once, in session order.
    let mut class_to_ws: HashMap<String, Vec<i32>> = HashMap::new();
    for ws_entry in workspaces {
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
        crate::debug!(
            "  fix: {} strayed to ws{} → moving to ws{target}",
            client.initial_class,
            client.workspace_id
        );
        hyprland::move_to_workspace_silent(&client.address, target)?;
        sleep(Duration::from_millis(100)).await;
        moved += 1;
    }

    if moved > 0 {
        println!(
            "{}: moved {moved} stray window(s) to correct workspace(s)",
            crate::color::hr()
        );
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

    // Apply col_width ratios, matching saved entries to live windows by class
    sleep(Duration::from_millis(200)).await;
    let live_final: Vec<LiveWindow> = hyprland::get_workspace_clients_sorted(ws_id)?
        .iter()
        .map(|c| LiveWindow {
            address: c.address.clone(),
            class: c.initial_class.clone(),
        })
        .collect();

    let saved_widths: Vec<(&str, f64)> = ws_entry
        .windows
        .iter()
        .map(|w| (w.class.as_str(), w.col_width))
        .collect();

    for (addr, width) in plan_width_assignments(&saved_widths, &live_final) {
        hyprland::focus_window(&addr)?;
        sleep(Duration::from_millis(200)).await;
        hyprland::colresize(width)?;
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

    #[test]
    fn widths_assigned_in_order_when_all_present() {
        let saved = [("a", 0.5), ("b", 0.3), ("c", 0.2)];
        let live = vec![lw("0x1", "a"), lw("0x2", "b"), lw("0x3", "c")];
        let ops = plan_width_assignments(&saved, &live);
        assert_eq!(
            ops,
            vec![
                ("0x1".to_owned(), 0.5),
                ("0x2".to_owned(), 0.3),
                ("0x3".to_owned(), 0.2),
            ]
        );
    }

    #[test]
    fn missing_middle_window_keeps_remaining_widths_aligned() {
        // Saved [a, b, c] but b failed to launch — c must still get c's width,
        // not b's (the old index-based zip applied b's width to c).
        let saved = [("a", 0.5), ("b", 0.3), ("c", 0.2)];
        let live = vec![lw("0x1", "a"), lw("0x3", "c")];
        let ops = plan_width_assignments(&saved, &live);
        assert_eq!(ops, vec![("0x1".to_owned(), 0.5), ("0x3".to_owned(), 0.2)]);
    }

    #[test]
    fn duplicate_classes_consumed_left_to_right() {
        let saved = [("ghostty", 0.6), ("ghostty", 0.4)];
        let live = vec![lw("0x1", "ghostty"), lw("0x2", "ghostty")];
        let ops = plan_width_assignments(&saved, &live);
        assert_eq!(ops, vec![("0x1".to_owned(), 0.6), ("0x2".to_owned(), 0.4)]);
    }

    #[test]
    fn extra_live_window_is_left_untouched() {
        let saved = [("a", 0.5)];
        let live = vec![lw("0x1", "a"), lw("0x2", "b")];
        let ops = plan_width_assignments(&saved, &live);
        assert_eq!(ops, vec![("0x1".to_owned(), 0.5)]);
    }

    fn win(class: &str, exe: &str) -> crate::session::WindowEntry {
        crate::session::WindowEntry {
            class: class.into(),
            exe: exe.into(),
            launch_args: None,
            col_width: 0.5,
        }
    }

    fn ws(windows: Vec<crate::session::WindowEntry>) -> crate::session::WorkspaceEntry {
        crate::session::WorkspaceEntry {
            workspace: 1,
            windows,
        }
    }

    #[test]
    fn plan_dedups_by_class_and_counts_saved() {
        let entry = ws(vec![
            win("ghostty", "/usr/bin/ghostty"),
            win("ghostty", "/usr/bin/ghostty"),
            win("firefox", "/usr/lib/firefox"),
        ]);
        let plans = plan_workspace(&entry, &HashMap::new(), &Config::default(), &[]);

        assert_eq!(plans.len(), 2, "duplicate class collapses to one plan");
        assert_eq!(plans[0].class, "ghostty", "column order preserved");
        assert_eq!(plans[0].saved_count, 2);
        assert_eq!(plans[0].needed, 2);
        assert_eq!(plans[1].class, "firefox");
        assert_eq!(plans[1].saved_count, 1);
    }

    #[test]
    fn plan_subtracts_pre_existing_and_saturates() {
        let entry = ws(vec![
            win("ghostty", "/usr/bin/ghostty"),
            win("ghostty", "/usr/bin/ghostty"),
            win("firefox", "/usr/lib/firefox"),
        ]);
        let pre = HashMap::from([("ghostty".to_owned(), 1), ("firefox".to_owned(), 3)]);
        let plans = plan_workspace(&entry, &pre, &Config::default(), &[]);

        assert_eq!(plans[0].pre, 1);
        assert_eq!(plans[0].needed, 1, "2 saved - 1 pre");
        assert_eq!(plans[1].needed, 0, "1 saved - 3 pre saturates to 0");
    }

    #[test]
    fn plan_flags_builtin_session_restore_apps() {
        let entry = ws(vec![
            win("firefox", "/usr/lib/firefox"),
            win("ghostty", "/usr/bin/ghostty"),
        ]);
        let plans = plan_workspace(&entry, &HashMap::new(), &Config::default(), &[]);

        assert!(plans[0].session_restore, "firefox is a built-in");
        assert!(!plans[1].session_restore, "ghostty is not");
    }

    #[test]
    fn plan_resolves_config_launch_args_over_session() {
        let mut cfg = Config::default();
        cfg.apps.insert(
            "firefox".to_owned(),
            crate::config::AppConfig {
                launch_args: vec!["--profile".to_owned(), "/work".to_owned()],
                session_restore: false,
            },
        );
        let mut window = win("firefox", "/usr/lib/firefox");
        window.launch_args = Some(vec!["--ignored".to_owned()]);
        let plans = plan_workspace(&ws(vec![window]), &HashMap::new(), &cfg, &[]);

        assert_eq!(plans[0].launch_args, vec!["--profile", "/work"]);
    }

    #[test]
    fn plan_trims_deleted_suffix_from_exe() {
        let entry = ws(vec![win("firefox", "/usr/lib/firefox (deleted)")]);
        let plans = plan_workspace(&entry, &HashMap::new(), &Config::default(), &[]);
        assert_eq!(plans[0].exe, "/usr/lib/firefox");
    }

    fn session_with(ws_ids: &[i32]) -> Session {
        Session {
            version: crate::session::SESSION_VERSION,
            active_workspace: ws_ids.first().copied().unwrap_or(1),
            workspaces: ws_ids
                .iter()
                .map(|&id| WorkspaceEntry {
                    workspace: id,
                    windows: vec![win("firefox", "/usr/lib/firefox")],
                })
                .collect(),
        }
    }

    #[test]
    fn select_none_returns_all_workspaces() {
        let session = session_with(&[1, 2, 3]);
        let selected = select_workspaces(&session, None).unwrap();
        assert_eq!(selected.len(), 3);
    }

    #[test]
    fn select_existing_workspace_returns_just_it() {
        let session = session_with(&[1, 2, 3]);
        let selected = select_workspaces(&session, Some(2)).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].workspace, 2);
    }

    #[test]
    fn select_missing_workspace_returns_none() {
        let session = session_with(&[1, 2, 3]);
        assert!(select_workspaces(&session, Some(9)).is_none());
    }
}
