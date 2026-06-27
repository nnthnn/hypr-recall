use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::VecDeque;
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

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
    let raw: RawActiveWorkspace = serde_json::from_slice(&out.stdout)
        .context("failed to parse hyprctl activeworkspace")?;
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
pub struct WindowOpenEvent {
    pub class: String,
}

pub async fn subscribe_openwindow() -> Result<mpsc::Receiver<WindowOpenEvent>> {
    let path = socket2_path()?;
    let stream = UnixStream::connect(&path)
        .await
        .with_context(|| format!("failed to connect to socket2: {path}"))?;
    let (tx, rx) = mpsc::channel(512);

    tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stream).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Some(ev) = parse_openwindow_line(&line) {
                if tx.send(ev).await.is_err() {
                    break;
                }
            }
        }
    });

    Ok(rx)
}

fn parse_openwindow_line(line: &str) -> Option<WindowOpenEvent> {
    let data = line.strip_prefix("openwindow>>")?;
    let mut parts = data.splitn(4, ',');
    let _addr_hex = parts.next()?;
    let _workspace = parts.next()?;
    let class = parts.next()?.to_string();
    Some(WindowOpenEvent { class })
}

pub struct EventStream {
    rx: mpsc::Receiver<WindowOpenEvent>,
    buffer: VecDeque<WindowOpenEvent>,
}

impl EventStream {
    pub fn new(rx: mpsc::Receiver<WindowOpenEvent>) -> Self {
        Self {
            rx,
            buffer: VecDeque::new(),
        }
    }

    /// Wait until `target_total` cumulative openwindow events for `class` have been
    /// received, or until `deadline` passes. Returns how many events were received.
    pub async fn wait_for_count(
        &mut self,
        class: &str,
        target_count: usize,
        deadline: Instant,
        // Optional child process to monitor for single-instance handoff detection
        mut child: Option<&mut tokio::process::Child>,
    ) -> usize {
        let mut found = 0;
        let mut child_done = child.is_none();
        let mut hard_deadline = deadline;

        // We need to handle the child separately since we can't hold two mutable borrows
        // in select!. Use a flag-based approach with try_wait.
        let spawn_time = Instant::now();

        while found < target_count {
            let now = Instant::now();
            if now >= hard_deadline {
                break;
            }
            let remaining = hard_deadline - now;

            // Check buffer first
            if let Some(pos) = self.buffer.iter().position(|e| e.class == class) {
                self.buffer.remove(pos);
                found += 1;
                continue;
            }

            // Check child exit (non-blocking) for handoff detection
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

            match tokio::time::timeout(remaining.min(Duration::from_millis(100)), self.rx.recv())
                .await
            {
                Ok(Some(ev)) => {
                    if ev.class == class {
                        found += 1;
                    } else {
                        self.buffer.push_back(ev);
                    }
                }
                Ok(None) => break, // channel closed
                Err(_) => {}       // timeout — loop to re-check child and deadline
            }
        }

        found
    }
}
