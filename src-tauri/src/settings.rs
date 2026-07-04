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
    /// Override the Claude Code config dir — the `.claude` folder holding
    /// `projects/` (transcripts) and `settings.json` (where hooks install).
    /// `None` = auto-resolve: the native home (`$HOME`/`%USERPROFILE%`), then on
    /// Windows a WSL-distro walk (`\\wsl$\<distro>\...\.claude`) for setups where
    /// Claude Code runs inside WSL and Whence on the host. Set this when discovery
    /// picks the wrong home (several distros/users with `.claude` trees).
    #[serde(default)]
    pub claude_dir: Option<String>,
    /// Explicit project-slug overrides keyed by Claude Code transcript **directory
    /// name** (e.g. `"-root-Projects-glue-mac" -> "glue-mac"`). Wins over the
    /// cwd- and dir-name-derived slug — the escape hatch for names the heuristics
    /// can't recover (the lossy `/`→`-` encoding, or a launch dir that differs
    /// from the canonical project slug). Empty by default.
    #[serde(default)]
    pub project_aliases: HashMap<String, String>,
    /// Override the loopback address the Claude Code hook receiver binds (and the
    /// URL written into the hook config). `None` = the built-in default
    /// (`127.0.0.1:18450`). Loopback-only by design — the receiver is unauthenticated
    /// (spec principle #4); bind it somewhere only local processes can reach.
    #[serde(default)]
    pub hook_listen_addr_override: Option<String>,
    /// Poll Ollama's local API for inference **liveness** (a low-confidence status
    /// signal only — never originates a focus switch; §5.2 / §14.5). **Opt-in,
    /// default off:** Ollama activity is *unattributed* (it knows inference is
    /// happening, not for what), so with the single status enum it can only paint
    /// the widget `active` with no project — which reads like a malfunction. Until
    /// temporal-correlation attribution lands, enable this only if you want the bare
    /// liveness signal.
    #[serde(default)]
    pub ollama_enabled: bool,
    /// Override the Ollama API origin. `None` = the built-in default
    /// (`http://localhost:11434`). Set this for a non-default host/port.
    #[serde(default)]
    pub ollama_endpoint: Option<String>,
    /// Receive cwd hints from a shell hook — a low-confidence **corroborator**
    /// (§5.3): it reinforces the current focus (and holds a block open while you
    /// work in the terminal with no AI activity) but never originates a switch.
    /// **Opt-in, default off:** it needs a one-line shell snippet POSTing `$PWD` to
    /// the loopback receiver (Whence never edits shell rc files), so enabling the
    /// flag alone does nothing until that hook is added.
    #[serde(default)]
    pub terminal_enabled: bool,
    /// Override the loopback address the terminal cwd receiver binds. `None` = the
    /// built-in default (`127.0.0.1:18451`). Loopback-only by design — the receiver
    /// is unauthenticated (principle #4).
    #[serde(default)]
    pub terminal_listen_addr_override: Option<String>,
    /// Receive browser LLM sessions from the first-party Whence extension — an
    /// **originating-capable** surface (self-attributes from the provider's project
    /// identity; §2). **Opt-in, default off:** it needs the extension installed and
    /// pointed at the loopback receiver, so enabling the flag alone does nothing until
    /// that's set up. The provider→slug mapping lives in `browser_mapping.toml`.
    #[serde(default)]
    pub browser_enabled: bool,
    /// Override the loopback address the browser receiver binds. `None` = the built-in
    /// default (`127.0.0.1:18452`). Loopback-only by design — the receiver is
    /// unauthenticated (principle #4); the extension POSTs to it from the page.
    #[serde(default)]
    pub browser_listen_addr_override: Option<String>,
    /// Sustained seconds before a focus switch is confirmed.
    pub switch_min_seconds: i64,
    /// Idle gap that ends a block.
    pub idle_timeout_seconds: i64,
    /// Confidence at/above which a signal is *primary* (can open/switch); below it
    /// it's a weak corroborator. ~0.6 (§7). Defaulted so existing settings files load.
    #[serde(default = "default_corroborator_cutoff")]
    pub corroborator_confidence_cutoff: f64,
    /// Seconds since your last act within which the active block reads *present* (vs
    /// *running*). ~120 (§7). Defaulted so existing settings files load.
    #[serde(default = "default_attention_recency")]
    pub attention_recency_seconds: i64,
    /// Whole-widget opacity, `0.3..=1.0` (1.0 = fully opaque). A *constant*
    /// user-set value — never adaptive — so the widget stays glanceable and calm
    /// (principle #3). Floored at 0.3 in the UI so it never becomes unreadable or
    /// effectively un-clickable. Frontend-only: applied to the `.panel`, no backend
    /// effect. Defaulted so existing settings files load.
    #[serde(default = "default_widget_opacity")]
    pub widget_opacity: f64,
    /// Float the widget above other windows. Default **true** (preserves the
    /// conf-set behavior). Applied to the main window at startup and on save.
    /// Cross-platform. Named default so settings files predating this field still
    /// load as on, not off (a bare `serde(default)` bool is false).
    #[serde(default = "default_always_on_top")]
    pub always_on_top: bool,
    /// Keep the widget in view across virtual desktops / workspaces. Opt-in,
    /// default false. Maps to Tauri's `set_visible_on_all_workspaces`.
    /// **Platform-specific:** macOS and Linux (libunity DEs, e.g. GNOME) only; a
    /// no-op on Windows/mobile.
    #[serde(default)]
    pub always_present: bool,
}

fn default_corroborator_cutoff() -> f64 {
    0.6
}

fn default_always_on_top() -> bool {
    true
}

fn default_attention_recency() -> i64 {
    120
}

fn default_widget_opacity() -> f64 {
    1.0
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            autostart: false,
            neuroskill_enabled: true,
            neuroskill_endpoint: None,
            neuroskill_token_path: None,
            neuroskill_data_dir: None,
            claude_dir: None,
            project_aliases: HashMap::new(),
            hook_listen_addr_override: None,
            ollama_enabled: false,
            ollama_endpoint: None,
            terminal_enabled: false,
            terminal_listen_addr_override: None,
            browser_enabled: false,
            browser_listen_addr_override: None,
            switch_min_seconds: 90,
            idle_timeout_seconds: 360,
            corroborator_confidence_cutoff: 0.6,
            attention_recency_seconds: 120,
            widget_opacity: 1.0,
            always_on_top: true,
            always_present: false,
        }
    }
}

/// Default loopback bind for the Claude Code hook receiver. Fixed port so the
/// install URL and the listener bind stay in lockstep without templating.
pub const DEFAULT_HOOK_ADDR: &str = "127.0.0.1:18450";

/// Default Ollama API origin — the local daemon's well-known address.
pub const DEFAULT_OLLAMA_ORIGIN: &str = "http://localhost:11434";

/// Default loopback bind for the terminal cwd receiver. Distinct fixed port from
/// the Claude hooks receiver (18450) so both can run at once.
pub const DEFAULT_TERMINAL_ADDR: &str = "127.0.0.1:18451";

/// Default loopback bind for the browser receiver. Distinct fixed port from the
/// hooks (18450) and terminal (18451) receivers so all three can run at once; the
/// extension POSTs to this address.
pub const DEFAULT_BROWSER_ADDR: &str = "127.0.0.1:18452";

impl Settings {
    pub fn segment_config(&self) -> crate::engine::segment::SegmentConfig {
        crate::engine::segment::SegmentConfig {
            switch_min_seconds: self.switch_min_seconds,
            idle_timeout_seconds: self.idle_timeout_seconds,
            corroborator_confidence_cutoff: self.corroborator_confidence_cutoff,
            attention_recency_seconds: self.attention_recency_seconds,
        }
    }

    /// The `host:port` the hook receiver binds — the override or the built-in
    /// loopback default.
    pub fn hook_listen_addr(&self) -> String {
        self.hook_listen_addr_override
            .clone()
            .unwrap_or_else(|| DEFAULT_HOOK_ADDR.to_string())
    }

    /// The `host:port` the terminal cwd receiver binds — the override or the
    /// built-in loopback default.
    pub fn terminal_listen_addr(&self) -> String {
        self.terminal_listen_addr_override
            .clone()
            .unwrap_or_else(|| DEFAULT_TERMINAL_ADDR.to_string())
    }

    /// The `host:port` the browser receiver binds — the override or the built-in
    /// loopback default.
    pub fn browser_listen_addr(&self) -> String {
        self.browser_listen_addr_override
            .clone()
            .unwrap_or_else(|| DEFAULT_BROWSER_ADDR.to_string())
    }

    /// The Ollama `/api/ps` URL to poll — the (override or default) origin with the
    /// endpoint path appended, trailing slash tolerated.
    pub fn ollama_ps_url(&self) -> String {
        let origin = self
            .ollama_endpoint
            .as_deref()
            .unwrap_or(DEFAULT_OLLAMA_ORIGIN)
            .trim_end_matches('/');
        format!("{origin}/api/ps")
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
