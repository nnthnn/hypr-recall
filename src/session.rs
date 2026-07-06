use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SESSION_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowEntry {
    pub class: String,
    pub exe: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_args: Option<Vec<String>>,
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
        let mut session: Self = serde_json::from_str(&text)?;
        if session.version > SESSION_VERSION {
            bail!(
                "session file version {} is newer than this build supports (current format is version {})\n\
                 Update hypr-recall to restore this session.",
                session.version,
                SESSION_VERSION
            );
        }
        migrate(&mut session)?;
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

    /// Replace, insert, or remove the `workspace_id` entry in `workspaces`.
    /// `entry: None` removes any existing entry for `workspace_id` (a no-op
    /// if it wasn't present). `entry: Some(..)` replaces the existing entry
    /// or inserts a new one, keeping `workspaces` sorted by workspace id.
    ///
    /// This does not assume `workspaces` is already sorted on entry — the
    /// session file can be hand-edited (e.g. via `hypr-recall edit`) with no
    /// re-validation, so a stale binary search over an unsorted or
    /// duplicate-id vec would silently insert at the wrong position. Instead
    /// this re-sorts by workspace id after inserting, which is correct
    /// regardless of the input order.
    pub fn merge_workspace(&mut self, workspace_id: i32, entry: Option<WorkspaceEntry>) {
        self.workspaces.retain(|w| w.workspace != workspace_id);
        if let Some(entry) = entry {
            self.workspaces.push(entry);
            self.workspaces.sort_by_key(|w| w.workspace);
        }
    }
}

/// Upgrades `session` in place from its stored version to `SESSION_VERSION`,
/// one step at a time. Add a match arm here whenever `SESSION_VERSION` is
/// bumped, transforming the fields that changed in that step.
fn migrate(session: &mut Session) -> Result<()> {
    while session.version < SESSION_VERSION {
        match session.version {
            // Pre-versioning session files (before this field existed) have
            // the same shape as v1 — nothing to transform, just stamp it.
            0 => {}
            v => bail!("no migration path from session version {v}"),
        }
        session.version += 1;
    }
    Ok(())
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
                            launch_args: None,
                            col_width: 0.989,
                        },
                        WindowEntry {
                            class: "com.mitchellh.ghostty".into(),
                            exe: "/usr/bin/ghostty".into(),
                            launch_args: None,
                            col_width: 0.493,
                        },
                    ],
                },
                WorkspaceEntry {
                    workspace: 2,
                    windows: vec![WindowEntry {
                        class: "dev.zed.Zed".into(),
                        exe: "/usr/bin/zed".into(),
                        launch_args: None,
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
    fn missing_version_field_migrates_to_current() {
        let path = std::env::temp_dir().join("hypr-recall-test-no-version.json");
        std::fs::write(&path, r#"{"active_workspace":1,"workspaces":[]}"#).unwrap();
        let session = Session::load(&path).unwrap();
        assert_eq!(session.version, SESSION_VERSION);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn migrated_session_round_trips_at_current_version() {
        let path = std::env::temp_dir().join("hypr-recall-test-migrate-round-trip.json");
        std::fs::write(
            &path,
            r#"{"active_workspace":1,"workspaces":[{"workspace":1,"windows":[{"class":"foo","exe":"/usr/bin/foo","col_width":0.5}]}]}"#,
        )
        .unwrap();
        let session = Session::load(&path).unwrap();
        session.save_to(&path).unwrap();
        let reloaded = Session::load(&path).unwrap();
        assert_eq!(reloaded.version, SESSION_VERSION);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn launch_args_round_trip() {
        let mut session = sample();
        session.workspaces[0].windows[0].launch_args = Some(vec![
            "--no-sandbox".to_owned(),
            "--profile".to_owned(),
            "/tmp/p".to_owned(),
        ]);
        let path = std::env::temp_dir().join("hypr-recall-test-launch-args.json");
        session.save_to(&path).unwrap();
        let loaded = Session::load(&path).unwrap();
        assert_eq!(
            loaded.workspaces[0].windows[0].launch_args,
            session.workspaces[0].windows[0].launch_args
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn launch_args_none_omitted_from_json() {
        let entry = WindowEntry {
            class: "foo".into(),
            exe: "/usr/bin/foo".into(),
            launch_args: None,
            col_width: 0.5,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("launch_args"));
    }

    #[test]
    fn launch_args_missing_in_json_loads_as_none() {
        let path = std::env::temp_dir().join("hypr-recall-test-no-launch-args.json");
        std::fs::write(
            &path,
            r#"{"version":1,"active_workspace":1,"workspaces":[{"workspace":1,"windows":[{"class":"foo","exe":"/usr/bin/foo","col_width":0.5}]}]}"#,
        ).unwrap();
        let session = Session::load(&path).unwrap();
        assert_eq!(session.workspaces[0].windows[0].launch_args, None);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn merge_workspace_replaces_existing_entry() {
        let mut session = sample();
        let replacement = WorkspaceEntry {
            workspace: 2,
            windows: vec![WindowEntry {
                class: "new-app".into(),
                exe: "/usr/bin/new-app".into(),
                launch_args: None,
                col_width: 0.75,
            }],
        };
        session.merge_workspace(2, Some(replacement.clone()));
        assert_eq!(session.workspaces.len(), 2);
        assert_eq!(session.workspaces[1], replacement);
    }

    #[test]
    fn merge_workspace_removes_entry_on_none() {
        let mut session = sample();
        session.merge_workspace(2, None);
        assert_eq!(session.workspaces.len(), 1);
        assert_eq!(session.workspaces[0].workspace, 1);
    }

    #[test]
    fn merge_workspace_none_on_missing_id_is_noop() {
        let mut session = sample();
        session.merge_workspace(99, None);
        assert_eq!(session.workspaces.len(), 2);
    }

    #[test]
    fn merge_workspace_inserts_sorted_between_existing() {
        let mut session = sample(); // workspaces [1, 2]
        session.workspaces[1].workspace = 5; // now [1, 5]
        let inserted = WorkspaceEntry {
            workspace: 2,
            windows: vec![],
        };
        session.merge_workspace(2, Some(inserted.clone()));
        assert_eq!(
            session
                .workspaces
                .iter()
                .map(|w| w.workspace)
                .collect::<Vec<_>>(),
            vec![1, 2, 5]
        );
    }

    #[test]
    fn merge_workspace_inserts_correctly_when_workspaces_are_unsorted() {
        // Simulates a hand-edited session file where `workspaces` is not
        // sorted ascending by id — nothing enforces that invariant on load.
        // A binary search (`partition_point`) over `[3, 1]` for an insert of
        // `2` is undefined behavior for sorted-input algorithms and returns
        // an incorrect position (verified empirically: it returns index 2,
        // producing `[3, 1, 2]` — workspace 2 landing at the very end,
        // after 3, instead of between 1 and 3). The fix must not depend on
        // the input already being sorted.
        let mut session = Session {
            version: SESSION_VERSION,
            active_workspace: 1,
            workspaces: vec![
                WorkspaceEntry {
                    workspace: 3,
                    windows: vec![],
                },
                WorkspaceEntry {
                    workspace: 1,
                    windows: vec![],
                },
            ],
        };
        let inserted = WorkspaceEntry {
            workspace: 2,
            windows: vec![],
        };
        session.merge_workspace(2, Some(inserted.clone()));
        assert_eq!(
            session
                .workspaces
                .iter()
                .map(|w| w.workspace)
                .collect::<Vec<_>>(),
            vec![1, 2, 3],
            "expected workspace 2 to land between 1 and 3 even though the \
             input vec was unsorted"
        );
    }

    #[test]
    fn merge_workspace_inserts_into_empty_session() {
        let mut session = Session {
            version: SESSION_VERSION,
            active_workspace: 1,
            workspaces: vec![],
        };
        let entry = WorkspaceEntry {
            workspace: 3,
            windows: vec![],
        };
        session.merge_workspace(3, Some(entry.clone()));
        assert_eq!(session.workspaces, vec![entry]);
    }
}
