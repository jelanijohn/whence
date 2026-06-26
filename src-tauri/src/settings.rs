//! User settings — a tiny JSON file in the app data dir. Surfaced in the widget;
//! the debounce tunables are read-only for v1 (calibrate, then expose).
//!
//! `autostart` is the only setting with an external side effect: toggling it
//! registers/unregisters the OS launch agent (handled in `commands::set_settings`).
//! It defaults to **false** — never default-on. A hand-launched sensor has gaps,
//! so autostart is justified, but the opt-in ethos means it stays explicit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Launch Whence at login. Opt-in; default false.
    pub autostart: bool,
    /// Write attribution labels into NeuroSkill on block open/close.
    pub neuroskill_enabled: bool,
    /// Override the NeuroSkill daemon HTTP origin. `None` = the built-in default
    /// (`http://127.0.0.1:18444`, which reaches the Windows-host daemon under
    /// WSL2 mirrored networking).
    #[serde(default)]
    pub neuroskill_endpoint: Option<String>,
    /// Override the daemon bearer-token file path. `None` = auto-resolve (native
    /// config dir, then WSL2 Windows-host discovery). Set this when the daemon's
    /// token lives somewhere non-standard — e.g. a specific
    /// `/mnt/c/Users/<you>/AppData/Roaming/skill/daemon/auth.token`.
    #[serde(default)]
    pub neuroskill_token_path: Option<String>,
    /// Override the NeuroSkill **data directory** (the folder holding
    /// `activity.sqlite`, the EEG store). `None` = auto-resolve (native local-data
    /// dir, then WSL2 Windows-host discovery under `AppData/Local/NeuroSkill`).
    /// Powers the optional read-only intensity meter; unrelated to the label write
    /// path, which uses the token path above.
    #[serde(default)]
    pub neuroskill_data_dir: Option<String>,
    /// Explicit project-slug overrides keyed by Claude Code transcript **directory
    /// name** (e.g. `"-root-Projects-glue-mac" -> "glue-mac"`). Wins over the
    /// cwd- and dir-name-derived slug — the escape hatch for names the heuristics
    /// can't recover (the lossy `/`→`-` encoding, or a launch dir that differs
    /// from the canonical project slug). Empty by default.
    #[serde(default)]
    pub project_aliases: HashMap<String, String>,
    /// Sustained seconds before a focus switch is confirmed.
    pub switch_min_seconds: i64,
    /// Idle gap that ends a block.
    pub idle_timeout_seconds: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            autostart: false,
            neuroskill_enabled: true,
            neuroskill_endpoint: None,
            neuroskill_token_path: None,
            neuroskill_data_dir: None,
            project_aliases: HashMap::new(),
            switch_min_seconds: 90,
            idle_timeout_seconds: 360,
        }
    }
}

impl Settings {
    pub fn segment_config(&self) -> crate::engine::segment::SegmentConfig {
        crate::engine::segment::SegmentConfig {
            switch_min_seconds: self.switch_min_seconds,
            idle_timeout_seconds: self.idle_timeout_seconds,
        }
    }
}

fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

/// Load settings, falling back to defaults on a missing or unreadable file.
pub fn load(data_dir: &Path) -> Settings {
    let path = settings_path(data_dir);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist settings (pretty JSON, so the file stays hand-inspectable).
pub fn save(data_dir: &Path, settings: &Settings) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let json = serde_json::to_string_pretty(settings).map_err(std::io::Error::other)?;
    std::fs::write(settings_path(data_dir), json)
}
