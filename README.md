# hypr-recall

Save and restore Hyprland window sessions, with first-class support for [hyprscroller](https://github.com/gudjonragnar/hyprscroller) column order and width.

No existing tool restores column positions and widths in scrolling layouts — hypr-recall does.

## Install

```fish
cargo build --release
cp target/release/hypr-recall ~/.local/bin/
```

## Usage

```fish
hypr-recall save              # snapshot current session
hypr-recall restore           # restore saved session
hypr-recall save --file ~/my-session.json
hypr-recall restore --file ~/my-session.json
```

Default session file: `~/.local/share/hypr-recall/session.json`

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

## Autostart

To auto-restore on login and auto-save on logout, add to your Hyprland config:

```lua
-- Restore session on login (only if no windows are open yet)
hl.exec("hypr-recall restore")

-- Save session before logout
-- (wire this to your logout keybind or shutdown hook)
hl.exec("hypr-recall save")
```

Or with a keybind:

```lua
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

## Requirements

- Hyprland with Lua config
- [hyprscroller](https://github.com/gudjonragnar/hyprscroller) plugin (for `swapcol` and `colresize`)
- `hyprctl` in PATH
- `HYPRLAND_INSTANCE_SIGNATURE` environment variable set (always true inside Hyprland)

## Limitations

- Floating windows are not saved or restored
- Apps that don't support being relaunched to a specific file/state will open a blank window (this is inherent to any session restore tool)
- Firefox and similar session-restore apps will open their previously saved session, not necessarily the same tabs as when `hypr-recall save` was run
