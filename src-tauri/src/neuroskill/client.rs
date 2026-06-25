//! The single **write** to NeuroSkill: fire its local `label` command over HTTP.
//!
//! Lifted from WAID's `neuroskill/client.rs`. The daemon exposes the same command
//! set over a WebSocket and a plain HTTP `POST /`, both gated by a bearer token it
//! writes to `<config>/skill/daemon/auth.token`. We use HTTP (simpler and more
//! robust than a hand-rolled WS upgrade) via `reqwest`. We never touch
//! `labels.sqlite` directly — the daemon owns it and `label` is the sanctioned path.
//!
//! Like Slack/Linear, the daemon can report a command-level failure as HTTP 200
//! with `{"ok": false}`, so we check the body as well as the status.
//!
//! Best-effort: a failure here (daemon down, wrong token) must never block focus
//! tracking — the caller logs it and carries on. NeuroSkill missing just means the
//! attribution labels aren't written this session; the timeline still records the
//! block.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// POST `{"command":"label","text":<text>}` to the daemon. `base` is the HTTP
/// origin (e.g. `http://127.0.0.1:18444`); `token`, when present, is sent as a
/// bearer credential (the daemon rejects unauthenticated calls with 401).
pub async fn fire_label(base: &str, token: Option<&str>, text: &str) -> Result<(), String> {
    let url = format!("{}/", base.trim_end_matches('/'));
    let mut req = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({ "command": "label", "text": text }));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("NeuroSkill isn't reachable at {url} (is the daemon running?): {e}"))?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(
            "NeuroSkill rejected the request (401) — the daemon auth token is missing or wrong."
                .to_string(),
        );
    }
    if !status.is_success() {
        return Err(format!("NeuroSkill returned HTTP {}.", status.as_u16()));
    }

    // 200 OK still doesn't guarantee success — the daemon reports command-level
    // failure in-band as `{"ok": false, ...}`.
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("NeuroSkill sent an unreadable response: {e}"))?;
    if body.get("ok").and_then(Value::as_bool) == Some(false) {
        let err = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("NeuroSkill couldn't record the label: {err}."));
    }
    Ok(())
}

/// The daemon's token file, relative to its config root.
const TOKEN_REL: &str = "skill/daemon/auth.token";

/// Resolve the daemon bearer-token file path on the **native** OS config dir
/// (`<config>/skill/daemon/auth.token`), matching WAID's default. `None` if no
/// config/home dir resolves.
pub fn default_token_path() -> Option<PathBuf> {
    let config = if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(x)
    } else if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("APPDATA")?)
    } else if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support")
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".config")
    };
    Some(config.join(TOKEN_REL))
}

/// Are we running inside WSL? The daemon then typically runs on the Windows host,
/// so its token lives under `/mnt/<drive>/Users/.../AppData/Roaming`, not the
/// Linux config dir.
fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("microsoft") || s.contains("wsl")
        })
        .unwrap_or(false)
}

/// Discover the Windows-host daemon token from inside WSL2 by walking
/// `/mnt/<drive>/Users/<user>/AppData/Roaming/skill/daemon/auth.token`. Returns
/// the first existing match (drives and users sorted for determinism).
fn wsl_token_path() -> Option<PathBuf> {
    let mnt = Path::new("/mnt");
    let mut drives: Vec<PathBuf> = std::fs::read_dir(mnt)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    drives.sort();
    for drive in drives {
        let users = drive.join("Users");
        let Ok(entries) = std::fs::read_dir(&users) else {
            continue;
        };
        let mut user_dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        user_dirs.sort();
        for user in user_dirs {
            let candidate = user.join("AppData/Roaming").join(TOKEN_REL);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Resolve the effective token-file path, honoring an explicit override first,
/// then the native config dir if it exists, then WSL2 Windows-host discovery.
/// `None` means we couldn't find a token file anywhere — we fire unauthenticated
/// (a no-auth daemon accepts it; an auth daemon 401s with a clear message).
pub fn resolve_token_path(override_path: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = override_path {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if let Some(p) = default_token_path() {
        if p.is_file() {
            return Some(p);
        }
    }
    if is_wsl() {
        if let Some(p) = wsl_token_path() {
            return Some(p);
        }
    }
    default_token_path()
}

/// Load the daemon bearer token (trimmed), honoring an explicit path override.
/// `None` if no path resolves or the file is missing/empty.
pub fn load_token(override_path: Option<&str>) -> Option<String> {
    let p = resolve_token_path(override_path)?;
    let s = std::fs::read_to_string(p).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}
