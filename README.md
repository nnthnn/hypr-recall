<div align="center">
  <img src="assets/logo-transparent.png" alt="hypr-recall" width="120">

# hypr-recall

[![CI](https://github.com/nnthnn/hypr-recall/actions/workflows/ci.yml/badge.svg)](https://github.com/nnthnn/hypr-recall/actions/workflows/ci.yml)

**[nnthnn.github.io/hypr-recall](https://nnthnn.github.io/hypr-recall/)**
</div>

Save and restore Hyprland window sessions, with first-class support for Hyprland's scrolling layout column order and width.

No existing tool restores column positions and widths in scrolling layouts — hypr-recall does.\*

> *Your columns were exactly where you left them. Probably.*

## Install

The restore overlay is an optional second binary (`hypr-recall-overlay`) gated
behind the `overlay` cargo feature, which needs `gtk4` and `gtk4-layer-shell`
installed. Build with `--all-features` to include it, or drop the flag for the
core binary only.

**With [`just`](https://github.com/casey/just)** (simplest):
```fish
just install-all   # core + overlay into ~/.cargo/bin (needs gtk4 / gtk4-layer-shell)
just install       # core binary only
```

**User-local** (no elevated privileges):
```fish
cargo build --release --all-features
cp target/release/hypr-recall target/release/hypr-recall-overlay ~/.local/bin/
```

**System-wide:**
```fish
cargo build --release --all-features
sudo install -Dm755 target/release/hypr-recall target/release/hypr-recall-overlay /usr/local/bin/
```

## Usage

```fish
hypr-recall save                      # snapshot current session (default name)
hypr-recall save work                 # snapshot to a named session
hypr-recall restore                   # restore default session
hypr-recall restore work              # restore named session
hypr-recall list                      # list all saved sessions with age and window counts
hypr-recall restore --dry-run         # preview what would be restored
hypr-recall restore --session-restore-app myapp  # treat myapp as a session-restore app
hypr-recall status                    # show saved session summary
hypr-recall edit                      # open session file in $EDITOR
hypr-recall save --file ~/my-session.json        # explicit path (overrides name)
```

Sessions are stored as `~/.local/share/hypr-recall/<name>.json`. The default name is `session`.

## What gets saved

For each tiled, non-floating, non-special window:

- **class** (`initialClass`) — used to identify app type
- **exe** (resolved from `/proc/<pid>/exe`) — used to relaunch
- **col_width** — column width as a ratio of monitor width (e.g. `0.5` = half screen)

Windows are stored in left-to-right order within each workspace. The active workspace is also saved.

Floating windows and special workspaces are excluded.

## How restore works

1. For each saved workspace (in order): switch to that workspace, then launch each app in saved column order
2. Waits for each app's window to appear via Hyprland's IPC event socket (`openwindow` events) — no polling
3. Apps like Firefox and Zed that restore their own sessions are launched once per workspace; single-instance handoff is detected and handled (if the process exits within 1.5s, wait up to 8s for the window)
4. After all windows for a workspace are open: reorder columns to match the saved left-to-right order using `swapcol l` dispatches, then resize each column to its saved width ratio
5. Finally, refocus the saved active workspace

## Hyprland config setup

### Auto-restore on login

Add to your `hyprland.lua` to restore your session when Hyprland starts:

```lua
hl.exec("hypr-recall restore")
```

### Auto-save on shutdown

Hook into Hyprland's shutdown event so the session is always saved on logout — no keybind required:

```lua
hl.on("hyprland.shutdown", function()
    os.execute("hypr-recall save")
end)
```

### Periodic autosave

Save every 10 minutes so a crash never loses more than a few minutes of layout work:

```lua
hl.timer(function()
    hl.exec("hypr-recall save")
end, { timeout = 600000, type = "repeat" })
```

### Manual keybinds

```lua
hl.bind("SUPER", "F11", "exec", "hypr-recall restore")
hl.bind("SUPER", "F12", "exec", "hypr-recall save")
```


## Session JSON format

```json
{
  "active_workspace": 3,
  "workspaces": [
    {
      "workspace": 1,
      "windows": [
        { "class": "firefox",       "exe": "/usr/lib/firefox/firefox",    "col_width": 0.989 },
        { "class": "sublime_text",  "exe": "/opt/sublime_text/sublime_text", "col_width": 0.493 }
      ]
    },
    {
      "workspace": 2,
      "windows": [
        { "class": "com.mitchellh.ghostty", "exe": "/usr/bin/ghostty", "col_width": 0.493 },
        { "class": "com.mitchellh.ghostty", "exe": "/usr/bin/ghostty", "col_width": 0.597 }
      ]
    }
  ]
}
```

## Config file

Optional config at `~/.config/hypr-recall/config.toml`:

```toml
overlay = true              # show spinning overlay with workspace progress during restore (default: true)
settle_delay_secs = 4       # seconds to wait before sweeping stray windows (default: 4)

# add apps to the session-restore list beyond the built-in set
session_restore_apps = ["my.electron.App"]

# per-app settings
[apps.firefox]
launch_args = ["--profile", "~/.mozilla/work"]

[apps.my.electron.App]
session_restore = true
```

If the file doesn't exist, all defaults apply. Unknown keys are rejected to catch typos.

## Development

The toolchain is pinned via `rust-toolchain.toml`. Common tasks are wrapped in a
[`justfile`](https://github.com/casey/just) — run `just` to list them:

```fish
just check       # fmt --check + clippy (-D warnings) + test  — run before every PR
just check-all   # same, plus the overlay feature (needs gtk4 / gtk4-layer-shell)
just fmt         # apply formatting
just audit       # cargo-deny advisory check (needs cargo-deny)
```

CI mirrors these: a core job (`fmt`/`clippy`/`test`), an overlay job that builds
and lints the `overlay` feature against gtk4, and a dependency advisory audit.

## Requirements

- Hyprland with Lua config
- Hyprland's scrolling layout (for `swapcol` and `colresize`)
- `hyprctl` in PATH
- `HYPRLAND_INSTANCE_SIGNATURE` environment variable set (always true inside Hyprland)

## Limitations

- Floating windows are not saved or restored
- Apps that don't support being relaunched to a specific file/state will open a blank window (this is inherent to any session restore tool)
- Firefox and similar session-restore apps will open their previously saved session, not necessarily the same tabs as when `hypr-recall save` was run

---

*\* as far as Claude Sonnet 4.6 could tell*
