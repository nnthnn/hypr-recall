use std::path::PathBuf;

/// Directories to search for `.desktop` files, in priority order (matches the
/// XDG data dir precedence users expect from other desktop tooling).
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    dirs.push(PathBuf::from("/usr/share/applications"));
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    dirs.push(PathBuf::from("/usr/local/share/applications"));
    dirs
}

/// Resolve a stable launch command for `class` by searching `dirs` for a
/// matching `.desktop` file. Tries `StartupWMClass=` first (case-insensitive,
/// since it's the field designed for exactly this window-class-to-app
/// mapping), then falls back to a `<class>.desktop` filename match for apps
/// whose desktop entry omits `StartupWMClass`.
pub fn resolve_by_class(class: &str, dirs: &[PathBuf]) -> Option<Vec<String>> {
    let entries = collect_desktop_files(dirs);

    for path in &entries {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        if find_key(&text, "StartupWMClass").is_some_and(|v| v.eq_ignore_ascii_case(class)) {
            if let Some(cmd) = find_key(&text, "Exec").and_then(|e| parse_exec(&e)) {
                return Some(cmd);
            }
        }
    }

    let filename_target = format!("{}.desktop", class.to_lowercase());
    for path in &entries {
        let matches_filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f.eq_ignore_ascii_case(&filename_target));
        if !matches_filename {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(cmd) = find_key(&text, "Exec").and_then(|e| parse_exec(&e)) {
                return Some(cmd);
            }
        }
    }

    None
}

fn collect_desktop_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
                out.push(path);
            }
        }
    }
    out
}

/// Find `key`'s value within the `[Desktop Entry]` section only, first match
/// wins (desktop files can have `[Desktop Action ...]` sections with their own
/// `Exec=`, which must not be confused with the main one).
fn find_key(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let mut in_main_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main_section = line == "[Desktop Entry]";
            continue;
        }
        if in_main_section {
            if let Some(rest) = line.strip_prefix(&prefix) {
                return Some(rest.trim().to_owned());
            }
        }
    }
    None
}

/// Strip desktop field codes (`%f`, `%F`, `%u`, `%U`, `%d`, `%D`, `%n`, `%N`,
/// `%i`, `%c`, `%k`, `%v`, `%m`) and unescape `%%`, then split into argv
/// respecting double-quoted segments.
fn parse_exec(exec: &str) -> Option<Vec<String>> {
    let cleaned = strip_field_codes(exec);
    let argv = split_exec(&cleaned);
    (!argv.is_empty()).then_some(argv)
}

fn strip_field_codes(exec: &str) -> String {
    let mut out = String::new();
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(&next) = chars.peek() {
                if next == '%' {
                    out.push('%');
                    chars.next();
                    continue;
                }
                if "fFuUdDnNickvm".contains(next) {
                    chars.next();
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

fn split_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in exec.trim().chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            ' ' if !in_quotes => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_desktop_file(dir: &std::path::Path, filename: &str, contents: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let mut f = std::fs::File::create(dir.join(filename)).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hypr-recall-desktop-entry-test-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn matches_by_startup_wm_class() {
        let dir = tmp_dir("wmclass");
        write_desktop_file(
            &dir,
            "discord.desktop",
            "[Desktop Entry]\nName=Discord\nExec=/usr/bin/discord --start-minimized\nStartupWMClass=discord\n",
        );
        let cmd = resolve_by_class("discord", &[dir]).unwrap();
        assert_eq!(cmd, vec!["/usr/bin/discord", "--start-minimized"]);
    }

    #[test]
    fn startup_wm_class_matches_case_insensitively() {
        let dir = tmp_dir("wmclass-case");
        write_desktop_file(
            &dir,
            "discord.desktop",
            "[Desktop Entry]\nExec=/usr/bin/discord\nStartupWMClass=Discord\n",
        );
        let cmd = resolve_by_class("discord", &[dir]).unwrap();
        assert_eq!(cmd, vec!["/usr/bin/discord"]);
    }

    #[test]
    fn falls_back_to_filename_when_no_wm_class_matches() {
        let dir = tmp_dir("filename-fallback");
        write_desktop_file(
            &dir,
            "ghostty.desktop",
            "[Desktop Entry]\nExec=/usr/bin/ghostty\n",
        );
        let cmd = resolve_by_class("ghostty", &[dir]).unwrap();
        assert_eq!(cmd, vec!["/usr/bin/ghostty"]);
    }

    #[test]
    fn strips_field_codes_from_exec() {
        let dir = tmp_dir("field-codes");
        write_desktop_file(
            &dir,
            "app.desktop",
            "[Desktop Entry]\nExec=/usr/bin/app %U --flag %f\nStartupWMClass=app\n",
        );
        let cmd = resolve_by_class("app", &[dir]).unwrap();
        assert_eq!(cmd, vec!["/usr/bin/app", "--flag"]);
    }

    #[test]
    fn no_match_returns_none() {
        let dir = tmp_dir("no-match");
        write_desktop_file(
            &dir,
            "other.desktop",
            "[Desktop Entry]\nExec=/usr/bin/other\nStartupWMClass=other\n",
        );
        assert!(resolve_by_class("discord", &[dir]).is_none());
    }

    #[test]
    fn missing_directory_is_skipped_not_an_error() {
        let missing = std::env::temp_dir().join("hypr-recall-desktop-entry-does-not-exist");
        assert!(resolve_by_class("discord", &[missing]).is_none());
    }

    #[test]
    fn search_dirs_includes_standard_locations() {
        let dirs = search_dirs();
        let strs: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        assert!(strs.iter().any(|d| d.ends_with("/usr/share/applications")));
        assert!(strs
            .iter()
            .any(|d| d.ends_with("/var/lib/flatpak/exports/share/applications")));
    }
}
