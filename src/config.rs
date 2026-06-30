use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

fn default_true() -> bool {
    true
}

fn default_settle_delay() -> u64 {
    4
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default)]
    pub launch_args: Vec<String>,
    /// Override whether this app is treated as a session-restore app.
    /// Useful for adding apps not in the built-in list without the CLI flag.
    #[serde(default)]
    pub session_restore: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Show the spinning overlay during restore. Default: true.
    #[serde(default = "default_true")]
    pub overlay: bool,
    /// Seconds to wait after restore before sweeping stray windows. Default: 4.
    #[serde(default = "default_settle_delay")]
    pub settle_delay_secs: u64,
    /// Extra app classes to treat as session-restore apps (in addition to the built-in list).
    #[serde(default)]
    pub session_restore_apps: Vec<String>,
    /// Per-app settings keyed by window class.
    #[serde(default)]
    pub apps: HashMap<String, AppConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            overlay: true,
            settle_delay_secs: 4,
            session_restore_apps: Vec::new(),
            apps: HashMap::new(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
    }

    /// Resolved launch args for a class: config takes precedence over session.json.
    pub fn launch_args<'a>(
        &'a self,
        class: &str,
        fallback: Option<&'a Vec<String>>,
    ) -> &'a [String] {
        if let Some(app) = self.apps.get(class) {
            if !app.launch_args.is_empty() {
                return &app.launch_args;
            }
        }
        fallback.map_or(&[], Vec::as_slice)
    }

    /// Returns true if `class` is a session-restore app (built-in list, config, or CLI extra).
    pub fn is_session_restore_app(
        &self,
        class: &str,
        builtin: &[&str],
        cli_extra: &[String],
    ) -> bool {
        builtin.contains(&class)
            || self.session_restore_apps.iter().any(|c| c == class)
            || self.apps.get(class).is_some_and(|a| a.session_restore)
            || cli_extra.iter().any(|c| c == class)
    }
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    PathBuf::from(home).join(".config/hypr-recall/config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Config {
        toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn defaults_when_empty() {
        let cfg = parse("");
        assert!(cfg.overlay);
        assert_eq!(cfg.settle_delay_secs, 4);
        assert!(cfg.session_restore_apps.is_empty());
        assert!(cfg.apps.is_empty());
    }

    #[test]
    fn overlay_can_be_disabled() {
        let cfg = parse("overlay = false");
        assert!(!cfg.overlay);
    }

    #[test]
    fn session_restore_apps_list() {
        let cfg = parse(r#"session_restore_apps = ["myapp", "another"]"#);
        assert!(cfg.is_session_restore_app("myapp", &[], &[]));
        assert!(cfg.is_session_restore_app("another", &[], &[]));
        assert!(!cfg.is_session_restore_app("unknown", &[], &[]));
    }

    #[test]
    fn per_app_session_restore_flag() {
        let cfg = parse("[apps.myapp]\nsession_restore = true");
        assert!(cfg.is_session_restore_app("myapp", &[], &[]));
    }

    #[test]
    fn launch_args_from_config() {
        let cfg = parse(
            r#"[apps.firefox]
launch_args = ["--profile", "/tmp/test"]"#,
        );
        assert_eq!(
            cfg.launch_args("firefox", None),
            &["--profile", "/tmp/test"]
        );
    }

    #[test]
    fn launch_args_fallback_to_session() {
        let cfg = parse("");
        let session_args = vec!["--flag".to_owned()];
        assert_eq!(cfg.launch_args("firefox", Some(&session_args)), &["--flag"]);
    }

    #[test]
    fn config_launch_args_override_session() {
        let cfg = parse(
            r#"[apps.firefox]
launch_args = ["--config-flag"]"#,
        );
        let session_args = vec!["--session-flag".to_owned()];
        assert_eq!(
            cfg.launch_args("firefox", Some(&session_args)),
            &["--config-flag"]
        );
    }

    #[test]
    fn builtin_restore_apps_still_work() {
        let cfg = parse("");
        let builtin = &["firefox", "discord"];
        assert!(cfg.is_session_restore_app("firefox", builtin, &[]));
        assert!(cfg.is_session_restore_app("discord", builtin, &[]));
    }

    #[test]
    fn cli_extra_still_works() {
        let cfg = parse("");
        let extra = vec!["myapp".to_owned()];
        assert!(cfg.is_session_restore_app("myapp", &[], &extra));
    }
}
