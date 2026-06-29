use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SESSION_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowEntry {
    pub class: String,
    pub exe: String,
    pub col_width: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    pub workspace: i32,
    pub windows: Vec<WindowEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub version: u32,
    pub active_workspace: i32,
    pub workspaces: Vec<WorkspaceEntry>,
}

impl Session {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let session: Self = serde_json::from_str(&text)?;
        if session.version != SESSION_VERSION {
            bail!(
                "session file version {} is not supported (current format is version {})\n\
                 Run 'hypr-recall save' to create a new session file.",
                session.version,
                SESSION_VERSION
            );
        }
        Ok(session)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Session {
        Session {
            version: SESSION_VERSION,
            active_workspace: 2,
            workspaces: vec![
                WorkspaceEntry {
                    workspace: 1,
                    windows: vec![
                        WindowEntry {
                            class: "firefox".into(),
                            exe: "/usr/lib/firefox/firefox".into(),
                            col_width: 0.989,
                        },
                        WindowEntry {
                            class: "com.mitchellh.ghostty".into(),
                            exe: "/usr/bin/ghostty".into(),
                            col_width: 0.493,
                        },
                    ],
                },
                WorkspaceEntry {
                    workspace: 2,
                    windows: vec![WindowEntry {
                        class: "dev.zed.Zed".into(),
                        exe: "/usr/bin/zed".into(),
                        col_width: 1.0,
                    }],
                },
            ],
        }
    }

    #[test]
    fn round_trip() {
        let session = sample();
        let path = std::env::temp_dir().join("hypr-recall-test-round-trip.json");
        session.save_to(&path).unwrap();
        let loaded = Session::load(&path).unwrap();
        assert_eq!(session, loaded);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn malformed_json_errors() {
        let path = std::env::temp_dir().join("hypr-recall-test-malformed.json");
        std::fs::write(&path, b"not json {{{").unwrap();
        assert!(Session::load(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_errors() {
        let path = std::env::temp_dir().join("hypr-recall-test-nonexistent-xyz.json");
        assert!(Session::load(&path).is_err());
    }

    #[test]
    fn wrong_version_errors() {
        let path = std::env::temp_dir().join("hypr-recall-test-version.json");
        std::fs::write(
            &path,
            r#"{"version":99,"active_workspace":1,"workspaces":[]}"#,
        )
        .unwrap();
        let err = Session::load(&path).unwrap_err();
        assert!(err.to_string().contains("version 99"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_version_field_errors() {
        let path = std::env::temp_dir().join("hypr-recall-test-no-version.json");
        std::fs::write(&path, r#"{"active_workspace":1,"workspaces":[]}"#).unwrap();
        let err = Session::load(&path).unwrap_err();
        assert!(err.to_string().contains("version 0"));
        std::fs::remove_file(&path).ok();
    }
}
