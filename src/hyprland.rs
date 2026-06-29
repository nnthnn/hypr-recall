use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashSet, VecDeque};
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
    id: i32,
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
    let raw: Vec<RawClient> =
        serde_json::from_slice(&out.stdout).context("failed to parse hyprctl clients")?;
    Ok(raw
        .into_iter()
        .map(|c| HyprClient {
            address: c.address,
            initial_class: c.initial_class,
            x: c.at[0],
            width: c.size[0],
            workspace_id: c.workspace.id,
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

fn socket2_path() -> Result<String> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .context("HYPRLAND_INSTANCE_SIGNATURE not set — are you running inside Hyprland?")?;
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        let p = format!("{xdg}/hypr/{sig}/.socket2.sock");
        if std::path::Path::new(&p).exists() {
            return Ok(p);
        }
    }
    Ok(format!("/tmp/hypr/{sig}/.socket2.sock"))
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
            if let Some(ev) = parse_event_line(&line) {
                if tx.send(ev).await.is_err() {
                    break;
                }
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
            if let Some(since) = stable_since {
                if now.duration_since(since) >= STABILITY_WINDOW {
                    break;
                }
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
            if !child_done {
                if let Some(ch) = child.as_deref_mut() {
                    if let Ok(Some(_)) = ch.try_wait() {
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
                    }
                    // Close events for untracked windows are irrelevant; don't buffer them.
                }
                Ok(None) => break,
                Err(_) => {}
            }
        }

        open_addresses.len()
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
}
