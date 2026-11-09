use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

/// How long the net window count must stay at `target` without a close event
/// before we declare success. Catches apps like Discord that open a splash/updater
/// window first (which fires openwindow), then close it once the real window appears.
const STABILITY_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct HyprClient {
    pub address: String,
    pub initial_class: String,
    pub x: i32,
    pub width: i32,
    pub workspace_id: i32,
    pub pid: i32,
    pub monitor: i32,
    pub floating: bool,
    pub mapped: bool,
}

#[derive(Deserialize)]
struct RawClient {
    address: String,
    mapped: bool,
    floating: bool,
    workspace: RawWorkspaceRef,
    at: [i32; 2],
    size: [i32; 2],
    #[serde(rename = "initialClass")]
    initial_class: String,
    pid: i32,
    monitor: i32,
}

#[derive(Deserialize)]
struct RawWorkspaceRef {
    /// Windows on an inactive special workspace come back without an `id` (the
    /// object only has `address`/`type`/`name`), so this must not be required —
    /// otherwise one scratchpad window makes the whole client list unparseable.
    /// Missing means "not a normal workspace", which is all callers need.
    #[serde(default)]
    id: Option<i32>,
}

#[derive(Deserialize)]
struct RawMonitor {
    id: i32,
    width: i32,
}

#[derive(Deserialize)]
struct RawActiveWorkspace {
    id: i32,
}

pub fn get_clients() -> Result<Vec<HyprClient>> {
    let out = Command::new("hyprctl")
        .args(["clients", "-j"])
        .output()
        .context("failed to run hyprctl clients")?;
    parse_clients(&out.stdout)
}

fn parse_clients(json: &[u8]) -> Result<Vec<HyprClient>> {
    let raw: Vec<RawClient> =
        serde_json::from_slice(json).context("failed to parse hyprctl clients")?;
    Ok(raw
        .into_iter()
        .map(|c| HyprClient {
            address: c.address,
            initial_class: c.initial_class,
            x: c.at[0],
            width: c.size[0],
            // Normal workspaces have positive ids; special/inactive ones are
            // either negative or absent, and every caller filters on `> 0` or a
            // positive saved id, so collapsing "absent" to 0 is safe.
            workspace_id: c.workspace.id.unwrap_or(0),
            pid: c.pid,
            monitor: c.monitor,
            floating: c.floating,
            mapped: c.mapped,
        })
        .collect())
}

pub fn get_monitor_widths() -> Result<std::collections::HashMap<i32, i32>> {
    let out = Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
        .context("failed to run hyprctl monitors")?;
    let raw: Vec<RawMonitor> =
        serde_json::from_slice(&out.stdout).context("failed to parse hyprctl monitors")?;
    Ok(raw.into_iter().map(|m| (m.id, m.width)).collect())
}

pub fn get_active_workspace_id() -> Result<i32> {
    let out = Command::new("hyprctl")
        .args(["activeworkspace", "-j"])
        .output()
        .context("failed to run hyprctl activeworkspace")?;
    let raw: RawActiveWorkspace =
        serde_json::from_slice(&out.stdout).context("failed to parse hyprctl activeworkspace")?;
    Ok(raw.id)
}

pub fn get_workspace_clients_sorted(ws_id: i32) -> Result<Vec<HyprClient>> {
    let mut clients: Vec<HyprClient> = get_clients()?
        .into_iter()
        .filter(|c| c.workspace_id == ws_id && !c.floating && c.mapped)
        .collect();
    clients.sort_by_key(|c| c.x);
    Ok(clients)
}

pub fn dispatch(lua_expr: &str) -> Result<()> {
    let out = Command::new("hyprctl")
        .args(["dispatch", lua_expr])
        .output()
        .with_context(|| format!("failed to dispatch: {lua_expr}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("dispatch failed: {stderr}");
    }
    Ok(())
}

pub fn focus_workspace(id: i32) -> Result<()> {
    dispatch(&format!("hl.dsp.focus({{workspace = {id}}})"))?;
    Ok(())
}

pub fn focus_window(address: &str) -> Result<()> {
    dispatch(&format!(
        "hl.dsp.focus({{ window = \"address:{address}\" }})"
    ))?;
    Ok(())
}

pub fn move_to_workspace_silent(address: &str, workspace_id: i32) -> Result<()> {
    dispatch(&format!(
        "hl.dsp.window.move({{workspace = {workspace_id}, follow = false, window = \"address:{address}\"}})"
    ))?;
    Ok(())
}

pub fn swapcol_left() -> Result<()> {
    dispatch("hl.dsp.layout(\"swapcol l\")")?;
    Ok(())
}

pub fn colresize(ratio: f64) -> Result<()> {
    dispatch(&format!("hl.dsp.layout(\"colresize {ratio:.3}\")"))?;
    Ok(())
}

/// The Hyprland operations the restore orchestration needs, behind a trait so
/// it can be driven by a fake in tests instead of a live compositor.
///
/// The production implementation is [`RealHyprland`], a thin delegate to the
/// free functions above. `sleep` is part of the trait deliberately: restore
/// settles the layout with short waits, and tests shouldn't have to spend wall
/// clock time on them.
pub trait Backend: Send + Sync {
    fn get_clients(&self) -> Result<Vec<HyprClient>>;
    fn get_monitor_widths(&self) -> Result<HashMap<i32, i32>>;
    fn get_workspace_clients_sorted(&self, ws_id: i32) -> Result<Vec<HyprClient>>;
    fn focus_workspace(&self, id: i32) -> Result<()>;
    fn focus_window(&self, address: &str) -> Result<()>;
    fn move_to_workspace_silent(&self, address: &str, workspace_id: i32) -> Result<()>;
    fn swapcol_left(&self) -> Result<()>;
    fn colresize(&self, ratio: f64) -> Result<()>;
    fn subscribe_events(&self) -> impl Future<Output = Result<mpsc::Receiver<HyprEvent>>> + Send;
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}

/// The real Hyprland backend, talking to the compositor over `hyprctl` and the
/// event socket.
pub struct RealHyprland;

impl Backend for RealHyprland {
    fn get_clients(&self) -> Result<Vec<HyprClient>> {
        get_clients()
    }

    fn get_monitor_widths(&self) -> Result<HashMap<i32, i32>> {
        get_monitor_widths()
    }

    fn get_workspace_clients_sorted(&self, ws_id: i32) -> Result<Vec<HyprClient>> {
        get_workspace_clients_sorted(ws_id)
    }

    fn focus_workspace(&self, id: i32) -> Result<()> {
        focus_workspace(id)
    }

    fn focus_window(&self, address: &str) -> Result<()> {
        focus_window(address)
    }

    fn move_to_workspace_silent(&self, address: &str, workspace_id: i32) -> Result<()> {
        move_to_workspace_silent(address, workspace_id)
    }

    fn swapcol_left(&self) -> Result<()> {
        swapcol_left()
    }

    fn colresize(&self, ratio: f64) -> Result<()> {
        colresize(ratio)
    }

    fn subscribe_events(&self) -> impl Future<Output = Result<mpsc::Receiver<HyprEvent>>> + Send {
        subscribe_events()
    }

    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(duration)
    }
}

/// Hyprland's per-instance runtime directory, which holds its IPC sockets and
/// `hyprland.lock`. The directory is created early in startup, before the
/// sockets and lock file it eventually contains.
fn instance_path() -> Option<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let xdg = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(|xdg| PathBuf::from(xdg).join("hypr").join(&sig));
    let tmp = PathBuf::from("/tmp/hypr").join(&sig);

    if let Some(path) = xdg.as_ref().filter(|path| path.exists()) {
        return Some(path.clone());
    }
    if tmp.exists() {
        return Some(tmp);
    }
    // Neither candidate exists yet (a restore started during Hyprland's own
    // startup). Prefer the XDG location, which is where Hyprland will create it.
    xdg.or(Some(tmp))
}

fn socket2_path() -> Result<String> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .context("HYPRLAND_INSTANCE_SIGNATURE not set — are you running inside Hyprland?")?;
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let p = format!("{xdg}/hypr/{sig}/.socket2.sock");
        if Path::new(&p).exists() {
            return Ok(p);
        }
    }
    // Last-resort guess, not verified to exist: if XDG_RUNTIME_DIR was unset
    // or the socket wasn't found there, the caller's connect() will fail
    // with a clear "failed to connect to socket2" error anyway.
    Ok(format!("/tmp/hypr/{sig}/.socket2.sock"))
}

/// How long to wait for Hyprland to finish starting when a restore is launched
/// from a config-time exec, and how often to check.
const COMPOSITOR_READY_TIMEOUT: Duration = Duration::from_secs(15);
const COMPOSITOR_READY_POLL: Duration = Duration::from_millis(50);

/// The Wayland display name Hyprland records in `hyprland.lock` once it is
/// ready. The file is `<pid>\n<wayland display>\n`.
fn wayland_display_from_lock(lock_path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(lock_path).ok()?;
    let display = contents.lines().nth(1)?.trim();
    (!display.is_empty()).then(|| display.to_owned())
}

/// Wait until Hyprland is far enough through startup for a restore to work,
/// returning the Wayland display children should use.
///
/// A command run from the top level of `hyprland.lua` is spawned while the
/// config is parsed — before Hyprland creates its Wayland socket and before it
/// starts answering `hyprctl`. The executor also exports `WAYLAND_DISPLAY` from
/// a field that is still empty at that point, so the child cannot talk to the
/// compositor even after Hyprland has finished starting. Waiting for
/// `hyprland.lock` (written by `startCompositor`) and reading the socket name
/// back out of it closes that race: a restore launched too early starts once
/// Hyprland is usable instead of silently failing to show its overlay and
/// launch its apps.
///
/// A manual restore is unaffected: when `WAYLAND_DISPLAY` is set and socket1
/// already exists, this returns immediately. If Hyprland never appears, the
/// timeout expires and the caller falls back to the inherited environment; the
/// `hyprctl` calls that follow then report the real error.
pub async fn wait_for_compositor() -> Option<String> {
    let inherited = std::env::var("WAYLAND_DISPLAY")
        .ok()
        .filter(|display| !display.is_empty());
    let Some(instance) = instance_path() else {
        return inherited;
    };

    let socket1 = instance.join(".socket.sock");
    let lock_path = instance.join("hyprland.lock");
    let deadline = Instant::now() + COMPOSITOR_READY_TIMEOUT;
    let mut waited = false;

    loop {
        let display = inherited
            .clone()
            .or_else(|| wayland_display_from_lock(&lock_path));
        if display.is_some() && socket1.exists() {
            if waited {
                crate::debug!(
                    "  Hyprland became ready — using WAYLAND_DISPLAY={}",
                    display.as_deref().unwrap_or_default()
                );
            }
            return display;
        }

        if Instant::now() >= deadline {
            crate::debug!(
                "  Hyprland was not ready after {}s — continuing with the inherited \
                 WAYLAND_DISPLAY (restore may fail to launch apps)",
                COMPOSITOR_READY_TIMEOUT.as_secs()
            );
            return display;
        }

        if !waited {
            crate::debug!(
                "  Hyprland is still starting — waiting up to {}s for its sockets",
                COMPOSITOR_READY_TIMEOUT.as_secs()
            );
            waited = true;
        }
        tokio::time::sleep(COMPOSITOR_READY_POLL).await;
    }
}

#[derive(Debug, Clone)]
pub enum HyprEvent {
    WindowOpen { address: String, class: String },
    WindowClose { address: String },
}

pub async fn subscribe_events() -> Result<mpsc::Receiver<HyprEvent>> {
    let path = socket2_path()?;
    let stream = UnixStream::connect(&path)
        .await
        .with_context(|| format!("failed to connect to socket2: {path}"))?;
    let (tx, rx) = mpsc::channel(512);

    tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stream).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Some(ev) = parse_event_line(&line)
                && tx.send(ev).await.is_err()
            {
                break;
            }
        }
    });

    Ok(rx)
}

fn parse_event_line(line: &str) -> Option<HyprEvent> {
    if let Some(data) = line.strip_prefix("openwindow>>") {
        let mut parts = data.splitn(4, ',');
        let address = parts.next()?.to_owned();
        let _workspace = parts.next()?;
        let class = parts.next()?.to_owned();
        Some(HyprEvent::WindowOpen { address, class })
    } else {
        line.strip_prefix("closewindow>>")
            .map(|address| HyprEvent::WindowClose {
                address: address.to_owned(),
            })
    }
}

pub struct EventStream {
    rx: mpsc::Receiver<HyprEvent>,
    buffer: VecDeque<HyprEvent>,
}

impl EventStream {
    pub fn new(rx: mpsc::Receiver<HyprEvent>) -> Self {
        Self {
            rx,
            buffer: VecDeque::new(),
        }
    }

    /// Wait until the net count of open windows for `class` reaches `target_count`
    /// and remains stable for `STABILITY_WINDOW`, or until `deadline` passes.
    ///
    /// Tracks both openwindow and closewindow events so that apps like Discord —
    /// which open a splash/updater window first — are handled correctly: the splash
    /// opens (net=1), the real window opens (net=2), the splash closes (net=1, timer
    /// resets), then stability passes and we declare done.
    ///
    /// Returns the net window count at the time we stopped waiting.
    pub async fn wait_for_count(
        &mut self,
        class: &str,
        target_count: usize,
        deadline: Instant,
        mut child: Option<&mut tokio::process::Child>,
    ) -> usize {
        // Addresses of currently-open windows of `class` that we've tracked.
        let mut open_addresses: HashSet<String> = HashSet::new();
        let mut stable_since: Option<Instant> = None;
        let mut child_done = child.is_none();
        let mut hard_deadline = deadline;
        let spawn_time = Instant::now();

        loop {
            let now = Instant::now();
            if now >= hard_deadline {
                break;
            }

            // Declare done once the net count has been stable at target long enough.
            if let Some(since) = stable_since
                && now.duration_since(since) >= STABILITY_WINDOW
            {
                break;
            }

            let remaining = hard_deadline - now;

            // Drain buffered open events for our class first.
            if let Some(pos) = self
                .buffer
                .iter()
                .position(|e| matches!(e, HyprEvent::WindowOpen { class: c, .. } if c == class))
            {
                if let Some(HyprEvent::WindowOpen { address, .. }) = self.buffer.remove(pos) {
                    open_addresses.insert(address);
                    Self::update_stable(&open_addresses, target_count, &mut stable_since);
                }
                continue;
            }

            // Check child exit for single-instance handoff detection.
            if !child_done
                && let Some(ch) = child.as_deref_mut()
                && let Ok(Some(_)) = ch.try_wait()
            {
                child_done = true;
                let elapsed = spawn_time.elapsed();
                if elapsed < Duration::from_millis(1500) {
                    eprintln!(
                        "  {class}: quick exit ({}ms) — single-instance handoff, extending wait to 8s",
                        elapsed.as_millis()
                    );
                    hard_deadline = Instant::now() + Duration::from_secs(8);
                }
            }

            match tokio::time::timeout(remaining.min(Duration::from_millis(50)), self.rx.recv())
                .await
            {
                Ok(Some(HyprEvent::WindowOpen {
                    address,
                    class: ev_class,
                })) => {
                    if ev_class == class {
                        open_addresses.insert(address.clone());
                        Self::update_stable(&open_addresses, target_count, &mut stable_since);
                    } else {
                        self.buffer.push_back(HyprEvent::WindowOpen {
                            address,
                            class: ev_class,
                        });
                    }
                }
                Ok(Some(HyprEvent::WindowClose { address })) => {
                    if open_addresses.remove(&address) {
                        // A tracked window closed — reset stability and re-evaluate.
                        stable_since = None;
                        Self::update_stable(&open_addresses, target_count, &mut stable_since);
                    } else {
                        // Not tracked in this wait, but it may be a buffered open
                        // awaiting another class's turn. Drop the matching buffered
                        // open so a window that opened-and-closed before its turn
                        // isn't counted as still live when we get to its class.
                        self.invalidate_buffered_open(&address);
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
        }

        open_addresses.len()
    }

    /// Remove a buffered `WindowOpen` whose address matches `address`, if present.
    /// Called when a close arrives for a window we aren't yet tracking, so a
    /// short-lived window of a not-yet-processed class doesn't linger in the
    /// buffer and inflate that class's count later.
    fn invalidate_buffered_open(&mut self, address: &str) {
        self.buffer
            .retain(|e| !matches!(e, HyprEvent::WindowOpen { address: a, .. } if a == address));
    }

    fn update_stable(open: &HashSet<String>, target: usize, stable_since: &mut Option<Instant>) {
        if open.len() == target {
            stable_since.get_or_insert_with(Instant::now);
        } else {
            // Above OR below target — reset. When above target a splash window is still
            // open; the timer must not start until the net count settles exactly at target.
            *stable_since = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clients_on_normal_workspaces() {
        let json = br#"[
            {
                "address": "0xabc",
                "mapped": true,
                "floating": false,
                "workspace": { "address": "2", "id": 2, "type": "normal", "name": "2" },
                "at": [10, 20],
                "size": [800, 600],
                "initialClass": "firefox",
                "pid": 42,
                "monitor": 0
            }
        ]"#;
        let clients = parse_clients(json).unwrap();
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].workspace_id, 2);
        assert_eq!(clients[0].initial_class, "firefox");
    }

    #[test]
    fn parses_clients_on_inactive_special_workspace_without_id() {
        // Hyprland omits `id` for a window on an inactive special workspace.
        // This used to fail the entire clients parse, aborting restore before
        // the overlay could map a single frame.
        let json = br#"[
            {
                "address": "0xabc",
                "mapped": true,
                "floating": true,
                "workspace": { "address": "special:scratch", "type": "special", "name": "special:scratch" },
                "at": [0, 0],
                "size": [800, 600],
                "initialClass": "scratch",
                "pid": 42,
                "monitor": 0
            },
            {
                "address": "0xdef",
                "mapped": true,
                "floating": false,
                "workspace": { "address": "1", "id": 1, "type": "normal", "name": "1" },
                "at": [0, 0],
                "size": [800, 600],
                "initialClass": "firefox",
                "pid": 43,
                "monitor": 0
            }
        ]"#;
        let clients = parse_clients(json).unwrap();
        assert_eq!(clients.len(), 2);
        assert_eq!(clients[0].workspace_id, 0);
        assert_eq!(clients[1].workspace_id, 1);
    }

    #[test]
    fn parses_openwindow_event() {
        let ev = parse_event_line("openwindow>>0xdeadbeef,1,firefox,Mozilla Firefox").unwrap();
        assert!(matches!(ev, HyprEvent::WindowOpen { ref class, .. } if class == "firefox"));
    }

    #[test]
    fn parses_openwindow_captures_address() {
        let ev = parse_event_line("openwindow>>0xdeadbeef,1,firefox,Mozilla Firefox").unwrap();
        assert!(matches!(ev, HyprEvent::WindowOpen { ref address, .. } if address == "0xdeadbeef"));
    }

    #[test]
    fn parses_dotted_class_name() {
        let ev = parse_event_line("openwindow>>0x1,2,com.mitchellh.ghostty,Ghostty").unwrap();
        assert!(
            matches!(ev, HyprEvent::WindowOpen { ref class, .. } if class == "com.mitchellh.ghostty")
        );
    }

    #[test]
    fn parses_closewindow_event() {
        let ev = parse_event_line("closewindow>>0xdeadbeef").unwrap();
        assert!(matches!(ev, HyprEvent::WindowClose { ref address } if address == "0xdeadbeef"));
    }

    #[test]
    fn ignores_unrecognised_events() {
        assert!(parse_event_line("activewindow>>firefox,Firefox").is_none());
        assert!(parse_event_line("").is_none());
    }

    #[test]
    fn update_stable_resets_when_above_target() {
        // Simulates Discord: splash opens (net=1=target), real window opens (net=2>target),
        // splash closes (net=1=target again). Timer must NOT fire while net is above target.
        let mut open: HashSet<String> = HashSet::new();
        let mut stable_since: Option<Instant> = None;

        open.insert("splash".into());
        EventStream::update_stable(&open, 1, &mut stable_since);
        assert!(stable_since.is_some(), "timer should start at net=1=target");

        open.insert("real".into());
        EventStream::update_stable(&open, 1, &mut stable_since);
        assert!(stable_since.is_none(), "timer must reset when net=2>target");

        open.remove("splash");
        EventStream::update_stable(&open, 1, &mut stable_since);
        assert!(
            stable_since.is_some(),
            "timer should restart after splash closes (net=1=target)"
        );
    }

    #[test]
    fn title_with_commas_doesnt_affect_class() {
        let ev = parse_event_line("openwindow>>0x1,1,firefox,Page, with, commas").unwrap();
        assert!(matches!(ev, HyprEvent::WindowOpen { ref class, .. } if class == "firefox"));
    }

    #[test]
    fn invalidate_buffered_open_removes_matching_address() {
        let (_tx, rx) = mpsc::channel(4);
        let mut stream = EventStream::new(rx);
        stream.buffer.push_back(HyprEvent::WindowOpen {
            address: "0xA".to_owned(),
            class: "discord".to_owned(),
        });
        stream.buffer.push_back(HyprEvent::WindowOpen {
            address: "0xB".to_owned(),
            class: "discord".to_owned(),
        });

        stream.invalidate_buffered_open("0xA");

        assert_eq!(stream.buffer.len(), 1);
        assert!(matches!(
            stream.buffer.front(),
            Some(HyprEvent::WindowOpen { address, .. }) if address == "0xB"
        ));
    }

    #[test]
    fn invalidate_buffered_open_no_match_is_noop() {
        let (_tx, rx) = mpsc::channel(4);
        let mut stream = EventStream::new(rx);
        stream.buffer.push_back(HyprEvent::WindowOpen {
            address: "0xA".to_owned(),
            class: "discord".to_owned(),
        });

        stream.invalidate_buffered_open("0xZ");

        assert_eq!(stream.buffer.len(), 1);
    }

    #[test]
    fn reads_wayland_display_from_lock_file() {
        let dir =
            std::env::temp_dir().join(format!("hypr-recall-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hyprland.lock");

        std::fs::write(&path, "1234\nwayland-1\n").unwrap();
        assert_eq!(
            wayland_display_from_lock(&path),
            Some("wayland-1".to_owned())
        );

        // Before the Wayland socket exists Hyprland writes an empty second line.
        std::fs::write(&path, "1234\n\n").unwrap();
        assert_eq!(wayland_display_from_lock(&path), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_lock_file_has_no_display() {
        let path = std::env::temp_dir().join("hypr-recall-lock-does-not-exist-xyz");
        std::fs::remove_file(&path).ok();
        assert_eq!(wayland_display_from_lock(&path), None);
    }
}
