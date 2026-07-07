//! Persisted GUI state: theme, recently used files and default job options.
//! Stored as JSON in the platform config directory; every operation is best
//! effort — a missing or corrupt file just yields defaults.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 10;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
}

/// Default settings applied to newly added jobs.
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct JobDefaults {
    pub format_parquet: bool,
    pub layout_raw: bool,
    pub interval_ms: f64,
    pub relative_timestamp: bool,
    pub keep_can_id: bool,
    pub skip_unknown: bool,
    pub overwrite: bool,
    pub drop_empty: bool,
    /// Column names as Message::Signal[unit] instead of the signal name.
    pub columns_full: bool,
}

impl Default for JobDefaults {
    fn default() -> Self {
        Self {
            format_parquet: false,
            layout_raw: false,
            interval_ms: 100.0,
            relative_timestamp: true,
            keep_can_id: false,
            skip_unknown: true,
            overwrite: false,
            drop_empty: true,
            columns_full: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct AppConfig {
    pub theme: ThemeChoice,
    pub recent_dbcs: Vec<PathBuf>,
    pub recent_out_dirs: Vec<PathBuf>,
    /// Directory of the last BLF the user picked, used as the file-dialog
    /// starting point.
    pub last_blf_dir: Option<PathBuf>,
    pub defaults: JobDefaults,
}

fn config_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    Some(base?.join("blf_decoder").join("config.json"))
}

impl AppConfig {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        if let Some(dir) = path.parent()
            && std::fs::create_dir_all(dir).is_ok()
            && let Ok(text) = serde_json::to_string_pretty(self)
        {
            let _ = std::fs::write(path, text);
        }
    }

    pub fn remember_dbc(&mut self, path: &Path) {
        push_recent(&mut self.recent_dbcs, path);
    }

    pub fn remember_out_dir(&mut self, path: &Path) {
        push_recent(&mut self.recent_out_dirs, path);
    }
}

fn push_recent(list: &mut Vec<PathBuf>, path: &Path) {
    list.retain(|p| p != path);
    list.insert(0, path.to_path_buf());
    list.truncate(MAX_RECENT);
}
