use crate::config::Config;
use crate::hyprland::{self, EventStream};
use crate::lock::LockGuard;
use crate::session::{Session, WorkspaceEntry};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

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

/// How many times to try bringing a workspace's column order in line with the
/// saved layout before giving up and warning.
const REORDER_ATTEMPTS: usize = 3;
/// A measured column width within this fraction of the monitor of the requested
/// ratio counts as settled (the read-back differs slightly from the requested
/// value because of gaps/borders).
const WIDTH_TOLERANCE: f64 = 0.03;

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
/// by class (first occurrence wins for `exe`/`launch_args`).
///
/// `pre_existing` is the count of already-open windows per class, snapshotted
/// once before any workspace is restored. It is consumed as workspaces are
/// planned: a pre-existing window can only stand in for the saved window it
/// matches, so crediting it to every workspace that also contains the class
/// would subtract it more than once and launch too few windows overall. The
/// count is therefore decremented (down to zero) as each plan claims its share.
fn plan_workspace(
    ws_entry: &crate::session::WorkspaceEntry,
    pre_existing: &mut HashMap<String, usize>,
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

        // Claim at most `saved_count` pre-existing windows for this workspace,
        // carrying any surplus forward to later workspaces instead of losing it.
        let available = pre_existing.get(class).copied().unwrap_or(0);
        let pre = available.min(saved_count);
        if pre > 0 {
            if available == pre {
                pre_existing.remove(class);
            } else {
                pre_existing.insert(class.clone(), available - pre);
            }
        }

        plans.push(ClassPlan {
            class: class.clone(),
            exe: window.exe.trim_end_matches(" (deleted)").to_owned(),
            launch_args: cfg.launch_args(class, window.launch_args.as_ref()).to_vec(),
            saved_count,
            pre,
            needed: saved_count - pre,
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

/// Resolve a launch command for `class`/`exe`: use `exe` as-is if it still
/// exists on disk (the common case — nothing changed since save time),
/// otherwise fall back to a `.desktop` file lookup by window class, since
/// versioned install paths (e.g. Discord's `app-<version>/Discord`) break on
/// every app update even though the app itself is still installed.
fn resolve_launch_command(class: &str, exe: &str) -> Option<Vec<String>> {
    if Path::new(exe).exists() {
        return Some(vec![exe.to_owned()]);
    }
    crate::desktop_entry::resolve_by_class(class, &crate::desktop_entry::search_dirs())
}

/// Count live clients whose `initial_class` is `class`. This is the key a
/// session stores, and unlike the event stream's current-class field it's
/// stable, so it's the authority for "did the window we launched appear?".
fn count_class<B: hyprland::Backend>(backend: &B, class: &str) -> Result<usize> {
    Ok(backend
        .get_clients()?
        .into_iter()
        .filter(|c| c.initial_class == class)
        .count())
}

pub async fn run<B: hyprland::Backend>(
    path: &Path,
    extra_restore_apps: &[String],
    cfg: &Config,
    only_workspace: Option<i32>,
    backend: &B,
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
    crate::progress!("{}: restoring '{name}'{scope}", crate::color::hr());
    let mut overlay = if cfg.overlay { spawn_overlay() } else { None };

    let lock_path = path.with_file_name("restore.lock");
    let _lock = LockGuard::acquire(lock_path)?;

    // Snapshot pre-existing window counts by class, plus their addresses, before
    // we launch anything. The counts keep us from re-launching apps that are
    // already open; the addresses let `fix_stray_windows` tell windows this
    // restore created apart from ones that were already there, so it never
    // relocates a window the user deliberately had on another workspace.
    let (mut pre_existing, pre_existing_addresses): (HashMap<String, usize>, HashSet<String>) = {
        let clients = backend.get_clients()?;
        let mut map: HashMap<String, usize> = HashMap::new();
        let mut addresses: HashSet<String> = HashSet::new();
        for c in &clients {
            *map.entry(c.initial_class.clone()).or_default() += 1;
            addresses.insert(c.address.clone());
        }
        (map, addresses)
    };

    // Subscribe to openwindow events before launching anything (avoids race)
    let rx = backend.subscribe_events().await?;
    let mut events = EventStream::new(rx);

    let total_workspaces = workspaces.len();
    for (ws_idx, &ws_entry) in workspaces.iter().enumerate() {
        let ws_id = ws_entry.workspace;
        crate::progress!(
            "{}: restoring workspace {ws_id} ({} windows)",
            crate::color::hr(),
            ws_entry.windows.len()
        );
        if let Some(ref mut ov) = overlay {
            ov.send_progress(ws_idx + 1, total_workspaces).await;
        }

        // One workspace failing (a dispatch error, a vanished monitor, ...) must
        // not abandon the rest of the restore, so the whole per-workspace body
        // is confined here and reported rather than propagated.
        let result: Result<()> = async {
            backend.focus_workspace(ws_id)?;
            backend.sleep(Duration::from_millis(200)).await;

            for plan in plan_workspace(ws_entry, &mut pre_existing, cfg, extra_restore_apps) {
            let class = &plan.class;
            let needed = plan.needed;

            if needed == 0 {
                crate::debug!(
                    "  {class}: skipped (pre-existing covers all {})",
                    plan.saved_count
                );
                continue;
            }

            let before_total = backend.get_clients()?
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

            let Some(cmd) = resolve_launch_command(class, exe) else {
                eprintln!(
                    "{}: could not restore \"{class}\" — binary \"{exe}\" not found and no matching \
                     .desktop entry, skipping",
                    crate::color::hr_err()
                );
                continue;
            };

            if plan.session_restore {
                // Launch once; the app restores all its windows itself
                let spawned = tokio::process::Command::new(&cmd[0])
                    .args(&cmd[1..])
                    .args(&plan.launch_args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                let mut child = match spawned {
                    Ok(child) => child,
                    Err(e) => {
                        eprintln!(
                            "{}: could not restore \"{class}\" — failed to spawn {}: {e}, skipping",
                            crate::color::hr_err(),
                            cmd[0]
                        );
                        continue;
                    }
                };

                let deadline = Instant::now() + Duration::from_secs(20);
                let got = events
                    .wait_for_count(class, needed, deadline, Some(&mut child))
                    .await;
                // Same class-rename caveat as the per-window path above: only
                // trust a short count after checking initialClass in the client
                // list.
                let converged =
                    got >= needed || count_class(backend, class)? >= before_total + needed;
                crate::debug!(
                    "  {class}: {got}/{needed} windows appeared (total: {}){}",
                    before_total + got,
                    if converged { "" } else { " — timed out" }
                );
            } else {
                // Launch one at a time and wait for each window
                for launch_n in 1..=needed {
                    let current = backend.get_clients()?
                        .into_iter()
                        .filter(|c| &c.initial_class == class)
                        .count();

                    if current >= target_total {
                        crate::debug!("  {class} launch {launch_n}/{needed}: already at {current}");
                        continue;
                    }

                    let spawned = tokio::process::Command::new(&cmd[0])
                        .args(&cmd[1..])
                        .args(&plan.launch_args)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn();
                    let mut child = match spawned {
                        Ok(child) => child,
                        Err(e) => {
                            eprintln!(
                                "{}: could not restore \"{class}\" (launch {launch_n}/{needed}) — \
                                 failed to spawn {}: {e}, skipping",
                                crate::color::hr_err(),
                                cmd[0]
                            );
                            continue;
                        }
                    };

                    let deadline = Instant::now() + Duration::from_secs(20);
                    let got = events
                        .wait_for_count(class, 1, deadline, Some(&mut child))
                        .await;
                    // The event stream reports a window's current class, but a
                    // session stores initialClass; an app that renames its class
                    // after mapping never matches by event. Fall back to the
                    // authoritative client list (which exposes initialClass, the
                    // key we saved) before reporting a timeout.
                    if got == 0 && count_class(backend, class)? <= current {
                        eprintln!("  TIMEOUT waiting for {class} (launch {launch_n}/{needed})");
                    }
                }
            }

            backend.sleep(Duration::from_millis(300)).await;
            }

            // All windows for this workspace are open — reorder columns then apply widths
            reorder_columns(ws_id, ws_entry, backend).await?;
            Ok(())
        }
        .await;

        if let Err(e) = result {
            eprintln!(
                "{}: workspace {ws_id} failed: {e:#} — continuing with remaining workspaces",
                crate::color::hr_err(),
            );
        }
    }

    if let Err(e) = fix_stray_windows(
        &workspaces,
        cfg.settle_delay_secs,
        &pre_existing_addresses,
        backend,
    )
    .await
    {
        eprintln!(
            "{}: stray-window sweep failed: {e:#}",
            crate::color::hr_err()
        );
    }

    if let Some(ref mut ov) = overlay {
        ov.kill().await;
    }

    // For a single-workspace restore, end focused on that workspace rather than
    // jumping to the session's saved active workspace (which we didn't restore).
    if let Err(e) = backend.focus_workspace(only_workspace.unwrap_or(session.active_workspace)) {
        eprintln!(
            "{}: could not refocus workspace: {e:#}",
            crate::color::hr_err()
        );
    }
    crate::progress!("{}: restore complete", crate::color::hr());
    Ok(())
}

pub fn run_dry<B: hyprland::Backend>(
    path: &Path,
    extra_restore_apps: &[String],
    cfg: &Config,
    only_workspace: Option<i32>,
    backend: &B,
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

    let mut pre_existing: HashMap<String, usize> = {
        let clients = backend.get_clients()?;
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

        for plan in plan_workspace(ws_entry, &mut pre_existing, cfg, extra_restore_apps) {
            let class = &plan.class;
            let needed = plan.needed;

            let args_suffix = if plan.launch_args.is_empty() {
                String::new()
            } else {
                format!(" [args: {}]", plan.launch_args.join(" "))
            };

            if needed == 0 {
                println!("    {class:<40} → skip ({} already open)", plan.pre);
            } else {
                match resolve_launch_command(class, &plan.exe) {
                    None => {
                        println!("    {class:<40} → SKIP (binary missing, no .desktop match)");
                    }
                    Some(cmd) if cmd[0] != plan.exe => {
                        let resolved = &cmd[0];
                        if plan.session_restore {
                            println!(
                                "    {class:<40} → launch 1  [session-restore, waits for {needed} window{}, via {resolved} — recorded exe missing]{args_suffix}",
                                if needed == 1 { "" } else { "s" }
                            );
                        } else {
                            println!(
                                "    {class:<40} → launch {needed}  [via {resolved} — recorded exe missing]{args_suffix}"
                            );
                        }
                    }
                    Some(_) if plan.session_restore => {
                        println!(
                            "    {class:<40} → launch 1  [session-restore, waits for {needed} window{}]{args_suffix}",
                            if needed == 1 { "" } else { "s" }
                        );
                    }
                    Some(_) => {
                        println!("    {class:<40} → launch {needed}{args_suffix}");
                    }
                }
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
/// on. Walk the live clients this restore is responsible for and, for any whose
/// class belongs to a restored workspace but which ended up elsewhere, move it
/// to the expected workspace.
///
/// `workspaces` is the set actually restored, so a single-workspace restore only
/// ever sweeps windows toward that one workspace and never disturbs others.
/// `pre_existing` holds the addresses of windows that were already open before
/// the restore began; see `stray_target` for what is deliberately left alone.
async fn fix_stray_windows<B: hyprland::Backend>(
    workspaces: &[&WorkspaceEntry],
    settle_secs: u64,
    pre_existing: &HashSet<String>,
    backend: &B,
) -> Result<()> {
    // Wait for late-opening windows (e.g. Discord Friends sidebar) to appear
    // before we sweep. Without this, the sweep runs before Discord finishes.
    backend.sleep(Duration::from_secs(settle_secs)).await;

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

    let clients = backend.get_clients()?;
    let mut moved = 0usize;
    // Workspaces a stray window was moved into. Each was already ordered, so the
    // late arrival needs its columns re-ordered to land in the right place
    // rather than at the end.
    let mut affected: HashSet<i32> = HashSet::new();

    for client in &clients {
        let Some(target) = stray_target(client, &class_to_ws, pre_existing) else {
            continue;
        };
        crate::debug!(
            "  fix: {} strayed to ws{} → moving to ws{target}",
            client.initial_class,
            client.workspace_id
        );
        backend.move_to_workspace_silent(&client.address, target)?;
        backend.sleep(Duration::from_millis(100)).await;
        affected.insert(target);
        moved += 1;
    }

    if moved > 0 {
        crate::progress!(
            "{}: moved {moved} stray window(s) to correct workspace(s)",
            crate::color::hr()
        );
    }

    // Re-order every workspace that received a stray window: reorder_columns ran
    // before this sweep, so without this the late window would sit at the end of
    // the columns with the wrong width.
    if !affected.is_empty() {
        let by_id: HashMap<i32, &WorkspaceEntry> =
            workspaces.iter().map(|w| (w.workspace, *w)).collect();
        for ws_id in affected {
            let Some(entry) = by_id.get(&ws_id) else {
                continue;
            };
            if let Err(e) = reorder_columns(ws_id, entry, backend).await {
                eprintln!(
                    "{}: reordering workspace {ws_id} after the stray sweep failed: {e:#}",
                    crate::color::hr_err(),
                );
            }
        }
    }

    Ok(())
}

/// Decide whether a live client is a stray window this restore should move,
/// returning the workspace it belongs on, or `None` to leave it alone.
///
/// Only windows this restore plausibly created are swept:
///
/// - **pre-existing windows** (their address is in `pre_existing`) were open
///   before the restore started and may have been put on another workspace on
///   purpose, so they're never relocated;
/// - **floating windows** aren't part of the saved tiling layout at all;
/// - **special/scratchpad workspaces** have negative ids, which the session
///   format doesn't model, so a window there can never be a valid target.
fn stray_target(
    client: &hyprland::HyprClient,
    class_to_ws: &HashMap<String, Vec<i32>>,
    pre_existing: &HashSet<String>,
) -> Option<i32> {
    if client.floating || client.workspace_id <= 0 {
        return None;
    }
    if pre_existing.contains(&client.address) {
        return None;
    }
    let valid = class_to_ws.get(&client.initial_class)?;
    if valid.contains(&client.workspace_id) {
        return None;
    }
    valid.first().copied()
}

async fn reorder_columns<B: hyprland::Backend>(
    ws_id: i32,
    ws_entry: &crate::session::WorkspaceEntry,
    backend: &B,
) -> Result<()> {
    backend.sleep(Duration::from_millis(200)).await;

    let saved_classes: Vec<&str> = ws_entry.windows.iter().map(|w| w.class.as_str()).collect();

    // Order the columns, then verify. A `swapcol` dispatch can race the
    // compositor, so re-read the live order and retry a bounded number of times
    // rather than trusting a fixed sleep.
    for attempt in 1..=REORDER_ATTEMPTS {
        let mut live = live_windows(backend, ws_id)?;
        if columns_match(&saved_classes, &live) {
            break;
        }

        let ops = plan_column_swaps(&saved_classes, &mut live);
        if ops.is_empty() {
            break;
        }

        for (addr, steps) in ops {
            backend.focus_window(&addr)?;
            for _ in 0..steps {
                backend.swapcol_left()?;
                backend.sleep(Duration::from_millis(150)).await;
            }
        }
        backend.sleep(Duration::from_millis(200)).await;

        if attempt == REORDER_ATTEMPTS {
            let live = live_windows(backend, ws_id)?;
            if !columns_match(&saved_classes, &live) {
                eprintln!(
                    "{}: could not reorder workspace {ws_id} into the saved column order after \
                     {REORDER_ATTEMPTS} attempts",
                    crate::color::hr_err(),
                );
            }
        }
    }

    // Apply col_width ratios, matching saved entries to live windows by class,
    // then verify the widths settled and re-apply anything that didn't.
    let saved_widths: Vec<(&str, f64)> = ws_entry
        .windows
        .iter()
        .map(|w| (w.class.as_str(), w.col_width))
        .collect();

    let ops = plan_width_assignments(&saved_widths, &live_windows(backend, ws_id)?);
    for (addr, width) in &ops {
        apply_col_width(backend, addr, *width).await?;
    }

    let settled = live_width_ratios(backend, ws_id)?;
    for (addr, width) in &ops {
        let Some(got) = settled.get(addr) else {
            continue;
        };
        if (*got - *width).abs() <= WIDTH_TOLERANCE {
            continue;
        }
        crate::debug!("  width {width:.3} on {addr} settled at {got:.3} — re-applying");
        apply_col_width(backend, addr, *width).await?;
    }

    Ok(())
}

/// The live clients on `ws_id`, as `LiveWindow`s for the ordering/width planners.
fn live_windows<B: hyprland::Backend>(backend: &B, ws_id: i32) -> Result<Vec<LiveWindow>> {
    Ok(backend
        .get_workspace_clients_sorted(ws_id)?
        .iter()
        .map(|c| LiveWindow {
            address: c.address.clone(),
            class: c.initial_class.clone(),
        })
        .collect())
}

/// Focus `address` and resize its column to `width` (a fraction of the monitor).
async fn apply_col_width<B: hyprland::Backend>(
    backend: &B,
    address: &str,
    width: f64,
) -> Result<()> {
    backend.focus_window(address)?;
    backend.sleep(Duration::from_millis(200)).await;
    backend.colresize(width)?;
    backend.sleep(Duration::from_millis(200)).await;
    Ok(())
}

/// Each live window on `ws_id`, mapped to its current width as a fraction of its
/// monitor, so a `colresize` can be checked against the requested ratio.
fn live_width_ratios<B: hyprland::Backend>(
    backend: &B,
    ws_id: i32,
) -> Result<HashMap<String, f64>> {
    let monitors = backend.get_monitor_widths()?;
    Ok(backend
        .get_workspace_clients_sorted(ws_id)?
        .into_iter()
        .map(|c| {
            let monitor_width = monitors
                .get(&c.monitor)
                .copied()
                .filter(|w| *w > 0)
                .unwrap_or(1920);
            (c.address, f64::from(c.width) / f64::from(monitor_width))
        })
        .collect())
}

/// Whether the live columns are already in the saved class order.
///
/// Only the common prefix is checked, mirroring `plan_column_swaps`: if fewer
/// windows are live than saved (an app failed to launch) the missing tail is
/// ignored, and extra live windows are left alone at the end.
fn columns_match(saved: &[&str], live: &[LiveWindow]) -> bool {
    let n = saved.len().min(live.len());
    (0..n).all(|i| live[i].class == saved[i])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

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
        let plans = plan_workspace(&entry, &mut HashMap::new(), &Config::default(), &[]);

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
        let mut pre = HashMap::from([("ghostty".to_owned(), 1), ("firefox".to_owned(), 3)]);
        let plans = plan_workspace(&entry, &mut pre, &Config::default(), &[]);

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
        let plans = plan_workspace(&entry, &mut HashMap::new(), &Config::default(), &[]);

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
        let plans = plan_workspace(&ws(vec![window]), &mut HashMap::new(), &cfg, &[]);

        assert_eq!(plans[0].launch_args, vec!["--profile", "/work"]);
    }

    #[test]
    fn plan_trims_deleted_suffix_from_exe() {
        let entry = ws(vec![win("firefox", "/usr/lib/firefox (deleted)")]);
        let plans = plan_workspace(&entry, &mut HashMap::new(), &Config::default(), &[]);
        assert_eq!(plans[0].exe, "/usr/lib/firefox");
    }

    #[test]
    fn resolve_launch_command_uses_exe_when_it_exists() {
        let tmp = std::env::temp_dir().join("hypr-recall-restore-test-exe-exists");
        std::fs::write(&tmp, b"").unwrap();
        let exe = tmp.to_str().unwrap();
        let cmd = resolve_launch_command("whatever-class", exe);
        assert_eq!(cmd, Some(vec![exe.to_owned()]));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn resolve_launch_command_returns_none_when_exe_and_desktop_entry_both_missing() {
        let cmd = resolve_launch_command(
            "definitely-not-a-real-class-xyz",
            "/definitely/not/a/real/path/xyz",
        );
        assert_eq!(cmd, None);
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

    #[test]
    fn plan_consumes_pre_existing_across_workspaces() {
        // Same class on two workspaces with one instance already open. The
        // pre-existing window may only be credited once in total, otherwise
        // each workspace subtracts it and we launch too few windows.
        let mut pre = HashMap::from([("ghostty".to_owned(), 1)]);
        let ws1 = ws(vec![
            win("ghostty", "/usr/bin/ghostty"),
            win("ghostty", "/usr/bin/ghostty"),
        ]);
        let ws2 = ws(vec![
            win("ghostty", "/usr/bin/ghostty"),
            win("ghostty", "/usr/bin/ghostty"),
        ]);

        let plan1 = plan_workspace(&ws1, &mut pre, &Config::default(), &[]);
        let plan2 = plan_workspace(&ws2, &mut pre, &Config::default(), &[]);

        assert_eq!(plan1[0].needed, 1);
        assert_eq!(plan2[0].needed, 2);
        assert_eq!(plan1[0].needed + plan2[0].needed, 3, "4 saved - 1 pre");
    }

    #[test]
    fn plan_carries_surplus_pre_existing_forward() {
        // Three instances already open, but the first workspace only needs two:
        // the surplus must carry to the next workspace, not evaporate.
        let mut pre = HashMap::from([("ghostty".to_owned(), 3)]);
        let ws1 = ws(vec![
            win("ghostty", "/usr/bin/ghostty"),
            win("ghostty", "/usr/bin/ghostty"),
        ]);
        let ws2 = ws(vec![
            win("ghostty", "/usr/bin/ghostty"),
            win("ghostty", "/usr/bin/ghostty"),
        ]);

        let plan1 = plan_workspace(&ws1, &mut pre, &Config::default(), &[]);
        let plan2 = plan_workspace(&ws2, &mut pre, &Config::default(), &[]);

        assert_eq!(plan1[0].needed, 0);
        assert_eq!(plan2[0].needed, 1);
    }

    fn client(
        address: &str,
        class: &str,
        workspace: i32,
        floating: bool,
    ) -> crate::hyprland::HyprClient {
        crate::hyprland::HyprClient {
            address: address.into(),
            initial_class: class.into(),
            x: 0,
            width: 100,
            workspace_id: workspace,
            pid: 1,
            monitor: 0,
            floating,
            mapped: true,
        }
    }

    #[test]
    fn stray_target_moves_new_window_on_wrong_workspace() {
        let class_to_ws = HashMap::from([("ghostty".to_owned(), vec![1])]);
        let c = client("0xA", "ghostty", 2, false);
        assert_eq!(stray_target(&c, &class_to_ws, &HashSet::new()), Some(1));
    }

    #[test]
    fn stray_target_leaves_pre_existing_window_alone() {
        // A window that was already open before the restore must never be
        // relocated, even if its class matches and it's on another workspace.
        let class_to_ws = HashMap::from([("ghostty".to_owned(), vec![1])]);
        let c = client("0xA", "ghostty", 2, false);
        let pre_existing = HashSet::from(["0xA".to_owned()]);
        assert_eq!(stray_target(&c, &class_to_ws, &pre_existing), None);
    }

    #[test]
    fn stray_target_leaves_floating_window_alone() {
        let class_to_ws = HashMap::from([("ghostty".to_owned(), vec![1])]);
        let c = client("0xA", "ghostty", 2, true);
        assert_eq!(stray_target(&c, &class_to_ws, &HashSet::new()), None);
    }

    #[test]
    fn stray_target_leaves_special_workspace_window_alone() {
        let class_to_ws = HashMap::from([("ghostty".to_owned(), vec![1])]);
        let c = client("0xA", "ghostty", -99, false);
        assert_eq!(stray_target(&c, &class_to_ws, &HashSet::new()), None);
    }

    #[test]
    fn stray_target_ignores_unknown_class_and_correct_workspace() {
        let class_to_ws = HashMap::from([("ghostty".to_owned(), vec![1, 3])]);
        let unknown = client("0xA", "firefox", 2, false);
        assert_eq!(stray_target(&unknown, &class_to_ws, &HashSet::new()), None);
        let already_valid = client("0xB", "ghostty", 3, false);
        assert_eq!(
            stray_target(&already_valid, &class_to_ws, &HashSet::new()),
            None
        );
    }

    #[test]
    fn stray_target_uses_first_valid_workspace_for_multi_workspace_class() {
        let class_to_ws = HashMap::from([("ghostty".to_owned(), vec![1, 3])]);
        let c = client("0xA", "ghostty", 2, false);
        assert_eq!(stray_target(&c, &class_to_ws, &HashSet::new()), Some(1));
    }

    #[test]
    fn columns_match_checks_prefix_only() {
        let saved = ["a", "b", "c"];
        // Fewer live windows than saved: the missing tail is ignored.
        assert!(columns_match(&saved, &[lw("0x1", "a"), lw("0x2", "b")]));
        // Extra live windows after the saved prefix are left alone.
        assert!(columns_match(
            &saved,
            &[
                lw("0x1", "a"),
                lw("0x2", "b"),
                lw("0x3", "c"),
                lw("0x4", "z"),
            ]
        ));
        assert!(!columns_match(&saved, &[lw("0x1", "b"), lw("0x2", "a")]));
    }

    fn client_at(
        address: &str,
        class: &str,
        workspace: i32,
        x: i32,
    ) -> crate::hyprland::HyprClient {
        let mut c = client(address, class, workspace, false);
        c.x = x;
        c
    }

    fn ws_entry(id: i32, classes: &[&str]) -> WorkspaceEntry {
        WorkspaceEntry {
            workspace: id,
            windows: classes
                .iter()
                .map(|c| win(c, &format!("/usr/bin/{c}")))
                .collect(),
        }
    }

    /// Write `session` to a fresh per-test directory. A directory per test keeps
    /// the `restore.lock` each run creates next to the session from colliding
    /// with a test running in parallel.
    fn write_session(name: &str, session: &Session) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hypr-recall-restore-test-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        session.save_to(&path).unwrap();
        path
    }

    /// An in-memory stand-in for Hyprland so the restore orchestration can run
    /// without a compositor or spawning real apps. `swapcol_left` really
    /// reorders its clients, so column ordering converges and can be asserted
    /// the way a real restore would.
    struct MockBackend {
        clients: Mutex<Vec<hyprland::HyprClient>>,
        focused: Mutex<Option<String>>,
        fail_focus: Mutex<HashSet<i32>>,
        focused_workspaces: Mutex<Vec<i32>>,
        swaps: Mutex<usize>,
        /// `swapcol` calls to swallow, to prove reordering is verified and
        /// retried rather than fire-and-forget.
        drop_swaps: Mutex<usize>,
        /// `get_clients` call counter, used to introduce a window *after* the
        /// pre-existing snapshot.
        calls: Mutex<usize>,
        /// `(get_clients call number, client)` to add when that call happens.
        strays: Mutex<Vec<(usize, hyprland::HyprClient)>>,
        tx: Mutex<Option<mpsc::Sender<hyprland::HyprEvent>>>,
    }

    impl MockBackend {
        fn new(clients: Vec<hyprland::HyprClient>) -> Self {
            Self {
                clients: Mutex::new(clients),
                focused: Mutex::new(None),
                fail_focus: Mutex::new(HashSet::new()),
                focused_workspaces: Mutex::new(Vec::new()),
                swaps: Mutex::new(0),
                drop_swaps: Mutex::new(0),
                calls: Mutex::new(0),
                strays: Mutex::new(Vec::new()),
                tx: Mutex::new(None),
            }
        }

        fn schedule_stray(&self, call: usize, client: hyprland::HyprClient) {
            self.strays.lock().unwrap().push((call, client));
        }

        fn order(&self, ws_id: i32) -> Vec<String> {
            let mut clients: Vec<_> = self
                .clients
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.workspace_id == ws_id && !c.floating && c.mapped)
                .map(|c| (c.x, c.initial_class.clone()))
                .collect();
            clients.sort_by_key(|(x, _)| *x);
            clients.into_iter().map(|(_, class)| class).collect()
        }
    }

    impl hyprland::Backend for MockBackend {
        fn get_clients(&self) -> Result<Vec<hyprland::HyprClient>> {
            let n = {
                let mut calls = self.calls.lock().unwrap();
                *calls += 1;
                *calls
            };
            let mut clients = self.clients.lock().unwrap();
            let mut strays = self.strays.lock().unwrap();
            let mut due = Vec::new();
            strays.retain(|(call, c)| {
                if *call == n {
                    due.push(c.clone());
                    false
                } else {
                    true
                }
            });
            clients.extend(due);
            Ok(clients.clone())
        }

        fn get_monitor_widths(&self) -> Result<HashMap<i32, i32>> {
            Ok(HashMap::from([(0, 1920)]))
        }

        fn get_workspace_clients_sorted(&self, ws_id: i32) -> Result<Vec<hyprland::HyprClient>> {
            let mut clients: Vec<_> = self
                .clients
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.workspace_id == ws_id && !c.floating && c.mapped)
                .cloned()
                .collect();
            clients.sort_by_key(|c| c.x);
            Ok(clients)
        }

        fn focus_workspace(&self, id: i32) -> Result<()> {
            self.focused_workspaces.lock().unwrap().push(id);
            if self.fail_focus.lock().unwrap().contains(&id) {
                anyhow::bail!("mock: focusing workspace {id} failed");
            }
            Ok(())
        }

        fn focus_window(&self, address: &str) -> Result<()> {
            *self.focused.lock().unwrap() = Some(address.to_owned());
            Ok(())
        }

        fn move_to_workspace_silent(&self, address: &str, workspace_id: i32) -> Result<()> {
            let mut clients = self.clients.lock().unwrap();
            let max_x = clients
                .iter()
                .filter(|c| c.workspace_id == workspace_id)
                .map(|c| c.x)
                .max()
                .unwrap_or(0);
            if let Some(c) = clients.iter_mut().find(|c| c.address == address) {
                c.workspace_id = workspace_id;
                c.x = max_x + 10;
            }
            Ok(())
        }

        fn swapcol_left(&self) -> Result<()> {
            let swallowed = {
                let mut drop = self.drop_swaps.lock().unwrap();
                if *drop > 0 {
                    *drop -= 1;
                    true
                } else {
                    false
                }
            };
            if swallowed {
                return Ok(());
            }

            let focused = self.focused.lock().unwrap().clone();
            let mut clients = self.clients.lock().unwrap();
            let Some(idx) = focused.and_then(|a| clients.iter().position(|c| c.address == a))
            else {
                return Ok(());
            };
            let ws = clients[idx].workspace_id;
            let x = clients[idx].x;
            let left = clients
                .iter()
                .enumerate()
                .filter(|(_, c)| c.workspace_id == ws && !c.floating && c.mapped && c.x < x)
                .max_by_key(|(_, c)| c.x)
                .map(|(i, _)| i);
            if let Some(li) = left {
                let lx = clients[li].x;
                clients[idx].x = lx;
                clients[li].x = x;
                *self.swaps.lock().unwrap() += 1;
            }
            Ok(())
        }

        #[allow(clippy::cast_possible_truncation)] // mock: monitor widths are small
        fn colresize(&self, ratio: f64) -> Result<()> {
            let focused = self.focused.lock().unwrap().clone();
            let mut clients = self.clients.lock().unwrap();
            if let Some(c) = focused.and_then(|a| clients.iter_mut().find(|c| c.address == a)) {
                c.width = (ratio * 1920.0).round() as i32;
            }
            Ok(())
        }

        fn subscribe_events(
            &self,
        ) -> impl std::future::Future<Output = Result<mpsc::Receiver<hyprland::HyprEvent>>> + Send
        {
            let (tx, rx) = mpsc::channel(16);
            *self.tx.lock().unwrap() = Some(tx);
            async move { Ok(rx) }
        }

        fn sleep(&self, _duration: Duration) -> impl std::future::Future<Output = ()> + Send {
            std::future::ready(())
        }
    }

    #[test]
    fn count_class_counts_by_initial_class() {
        let mock = MockBackend::new(vec![
            client_at("0x1", "app", 1, 0),
            client_at("0x2", "app", 2, 0),
            client_at("0x3", "other", 1, 0),
        ]);
        assert_eq!(count_class(&mock, "app").unwrap(), 2);
        assert_eq!(count_class(&mock, "other").unwrap(), 1);
        assert_eq!(count_class(&mock, "missing").unwrap(), 0);
    }

    #[tokio::test]
    async fn reorder_retries_when_a_swap_does_not_take() {
        let entry = ws_entry(1, &["a", "b"]);
        let mock = MockBackend::new(vec![
            client_at("0xB", "b", 1, 0),
            client_at("0xA", "a", 1, 10),
        ]);
        // First swap is swallowed: a fire-and-forget reorder would give up with
        // the wrong order; the verify-and-retry loop must recover.
        *mock.drop_swaps.lock().unwrap() = 1;

        reorder_columns(1, &entry, &mock).await.unwrap();

        assert_eq!(mock.order(1), vec!["a", "b"]);
        assert!(
            *mock.swaps.lock().unwrap() >= 1,
            "a swap must eventually take effect"
        );
    }

    #[tokio::test]
    async fn run_continues_after_a_workspace_fails() {
        let session = Session {
            version: crate::session::SESSION_VERSION,
            active_workspace: 1,
            workspaces: vec![
                ws_entry(1, &["a"]),
                ws_entry(2, &["b"]),
                ws_entry(3, &["c"]),
            ],
        };
        let path = write_session("p8-continue", &session);
        let mock = MockBackend::new(vec![
            client_at("0xA", "a", 1, 0),
            client_at("0xB", "b", 2, 0),
            client_at("0xC", "c", 3, 0),
        ]);
        mock.fail_focus.lock().unwrap().insert(2);

        let cfg = Config {
            overlay: false,
            settle_delay_secs: 0,
            ..Config::default()
        };
        run(&path, &[], &cfg, None, &mock).await.unwrap();

        let focused = mock.focused_workspaces.lock().unwrap().clone();
        assert!(focused.contains(&2), "workspace 2 was attempted");
        assert!(
            focused.contains(&3),
            "workspace 3 still restored despite workspace 2 failing"
        );
        if let Some(dir) = path.parent() {
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[tokio::test]
    async fn stray_window_swept_into_a_workspace_is_reordered() {
        // Saved ws1 wants [a, b]. "a" isn't on ws1 at first — it appears late on
        // ws2 and is swept over. Without re-ordering after the sweep, ws1 would
        // settle as [b, a].
        let session = Session {
            version: crate::session::SESSION_VERSION,
            active_workspace: 1,
            workspaces: vec![ws_entry(1, &["a", "b"])],
        };
        let path = write_session("p3-stray-reorder", &session);
        let mock = MockBackend::new(vec![
            // A pre-existing "a" elsewhere: it counts toward the pre-existing
            // total (so nothing is launched) but must not itself be swept.
            client_at("0xPRE", "a", 3, 0),
            client_at("0xB", "b", 1, 0),
        ]);
        // The late "a" shows up on ws2 during the sweep's get_clients call (#2).
        mock.schedule_stray(2, client_at("0xLATE", "a", 2, 0));

        let cfg = Config {
            overlay: false,
            settle_delay_secs: 0,
            ..Config::default()
        };
        run(&path, &[], &cfg, None, &mock).await.unwrap();

        assert_eq!(mock.order(1), vec!["a", "b"]);
        assert_eq!(
            mock.order(3),
            vec!["a"],
            "the pre-existing window must be left where it was"
        );
        if let Some(dir) = path.parent() {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}
