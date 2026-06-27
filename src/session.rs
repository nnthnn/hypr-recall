use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowEntry {
    pub class: String,
    pub exe: String,
    pub col_width: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    pub workspace: i32,
    pub windows: Vec<WindowEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub active_workspace: i32,
    pub workspaces: Vec<WorkspaceEntry>,
}

impl Session {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
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
