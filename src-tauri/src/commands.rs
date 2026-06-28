//! Tauri commands — the frontend's only door into the backend. Thin: query the
//! shared focus snapshot, read the JSONL timeline, and get/set settings. The
//! `tauri.ts` binding layer mirrors these one-to-one.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tauri::State;
use tauri_plugin_autostart::ManagerExt;

use crate::orchestrator::SharedSnapshot;
use crate::engine::segment::{FocusBlock, FocusSnapshot};
use crate::engine::timeline;
use crate::neuroskill::health::{NeuroskillStatus, SharedStatus};
use crate::settings::{self, Settings};

/// Managed app state, shared across commands and the core task.
pub struct AppState {
    pub snapshot: SharedSnapshot,
    pub data_dir: PathBuf,
    pub settings: Arc<Mutex<Settings>>,
    /// Live NeuroSkill connection status — written by the health probe loop, read
    /// here for the widget's first paint (it then tracks the `whence://neuroskill`
    /// event).
    pub neuroskill: SharedStatus,
}

/// Current focus snapshot — every live session (each with its own status + timer).
/// A poisoned lock falls back to "no sessions" (idle) rather than panicking.
#[tauri::command]
pub fn get_focus_state(state: State<AppState>) -> FocusSnapshot {
    state
        .snapshot
        .lock()
        .map(|g| g.clone())
        .unwrap_or(FocusSnapshot { projects: Vec::new() })
}

/// Today's closed focus blocks, oldest first — drives the expanded timeline.
#[tauri::command]
pub fn get_today_blocks(state: State<AppState>) -> Result<Vec<FocusBlock>, String> {
    let path = timeline::timeline_path(&state.data_dir);
    timeline::read_day(&path, chrono::Utc::now().timestamp())
        .map_err(|e| format!("could not read timeline: {e}"))
}

/// Mean EEG focus (0..100) over the last couple of minutes — drives the widget's
/// optional intensity meter (spec §8 read-back). **Read-only** (principle #5):
/// resolves NeuroSkill's `activity.sqlite` and issues the single scoped
/// `eeg_timeseries` SELECT, nothing else. `None` when the `eeg-readback` feature is
/// off, NeuroSkill's store can't be found, or there are no recent epochs — the
/// meter simply hides, best-effort like the label write. Never errors.
#[tauri::command]
pub fn get_focus_intensity(state: State<AppState>) -> Option<f64> {
    #[cfg(feature = "eeg-readback")]
    {
        let data_dir_override = state
            .settings
            .lock()
            .ok()
            .and_then(|g| g.neuroskill_data_dir.clone());
        let db = crate::neuroskill::eeg::resolve_activity_db(data_dir_override.as_deref())?;
        let now = chrono::Utc::now().timestamp();
        // Last ~2 minutes of ~5s epochs — "live" without being jumpy.
        crate::neuroskill::eeg::mean_focus(&db, now - 120, now)
            .ok()
            .flatten()
    }
    #[cfg(not(feature = "eeg-readback"))]
    {
        let _ = &state; // feature off: SQLite isn't compiled in, so there's nothing to read.
        None
    }
}

/// Current NeuroSkill connection status — drives the header indicator's first paint.
/// The probe loop keeps this fresh and pushes changes on `whence://neuroskill`; a
/// poisoned lock falls back to `Unknown` rather than panicking.
#[tauri::command]
pub fn get_neuroskill_status(state: State<AppState>) -> NeuroskillStatus {
    state
        .neuroskill
        .lock()
        .map(|g| *g)
        .unwrap_or(NeuroskillStatus::Unknown)
}

/// Absolute path to the browser-adapter mapping file (`browser_mapping.toml`), for
/// the settings deep-link (§7). The file is hand-editable — re-slug a rename, fix a
/// bad mint — so the panel surfaces where it lives. Returns the path whether or not
/// it exists yet (the daemon creates it on first mint).
#[tauri::command]
pub fn get_browser_mapping_path(state: State<AppState>) -> String {
    crate::adapters::browser_map::mapping_path(&state.data_dir)
        .to_string_lossy()
        .into_owned()
}

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Settings {
    state
        .settings
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Persist settings. Toggling `autostart` registers/unregisters the OS launch
/// agent — the one setting with an external side effect, and the reason this
/// can't just be a file write. The debounce tunables take effect on next launch
/// (the running engine reads them at startup).
#[tauri::command]
pub fn set_settings(
    app: tauri::AppHandle,
    state: State<AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    // Reconcile autostart with the OS only when it actually changed, so we don't
    // re-register on every settings save.
    let prev_autostart = state
        .settings
        .lock()
        .map(|g| g.autostart)
        .unwrap_or(false);
    if settings.autostart != prev_autostart {
        let mgr = app.autolaunch();
        let res = if settings.autostart {
            mgr.enable()
        } else {
            mgr.disable()
        };
        res.map_err(|e| format!("could not update autostart: {e}"))?;
    }

    settings::save(&state.data_dir, &settings)
        .map_err(|e| format!("could not save settings: {e}"))?;
    if let Ok(mut g) = state.settings.lock() {
        *g = settings.clone();
    }
    Ok(settings)
}

/// The Claude Code lifecycle events Whence registers `http` hooks for. `Stop` and
/// the input-awaiting `Notification`s are what light the widget up as *waiting on
/// you*; `UserPromptSubmit` clears it; the session pair bookends activity. (Tool
/// events are deliberately omitted — transcript-watch already covers active-ness.)
const HOOK_EVENTS: [&str; 5] = [
    "UserPromptSubmit",
    "Stop",
    "Notification",
    "SessionStart",
    "SessionEnd",
];

/// Per-hook dispatch timeout (seconds). Whence's receiver answers instantly with an
/// empty `200`, so this only ever bites when the listener is down — keep it short so
/// a missing Whence never stalls a Claude Code turn.
const HOOK_TIMEOUT_SECS: u32 = 5;

/// Install (opt-in) the Claude Code `http` hooks that POST to Whence's loopback
/// receiver. **Merge-preserving** — reads `~/.claude/settings.json`, adds our hook
/// entries (idempotent, keyed by URL), and writes back without touching any other
/// keys. The spec's "offers to write the hook config; never silently" (§5.1): this
/// fires only on explicit user action.
#[tauri::command]
pub fn install_claude_hooks(state: State<AppState>) -> Result<(), String> {
    let url = hook_url(&state);
    let path = claude_settings_path()?;
    let existing = read_json_object(&path)?;
    let merged = merge_hook_config(existing, &url);
    write_json_pretty(&path, &merged)
}

/// Remove Whence's `http` hook entries from `~/.claude/settings.json`, leaving all
/// other config intact. Round-trips `install` exactly.
#[tauri::command]
pub fn uninstall_claude_hooks(state: State<AppState>) -> Result<(), String> {
    let url = hook_url(&state);
    let path = claude_settings_path()?;
    let existing = read_json_object(&path)?;
    let pruned = remove_hook_config(existing, &url);
    write_json_pretty(&path, &pruned)
}

/// The receiver URL written into the hook config — `http://<bind>/hook`, where the
/// bind is the (possibly overridden) loopback address the listener is using.
fn hook_url(state: &State<AppState>) -> String {
    let addr = state
        .settings
        .lock()
        .map(|g| g.hook_listen_addr())
        .unwrap_or_else(|_| settings::DEFAULT_HOOK_ADDR.to_string());
    format!("http://{addr}/hook")
}

/// `~/.claude/settings.json` — the user-scope config, so the hooks fire for *every*
/// Claude Code session regardless of project. Claude Code runs on the Linux side
/// under WSL2 (same place the transcript watcher reads `~/.claude/projects`), so
/// `$HOME` is the right root.
fn claude_settings_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("could not resolve $HOME")?;
    Ok(PathBuf::from(home).join(".claude").join("settings.json"))
}

/// Read a JSON object from `path`, defaulting to an empty object when the file is
/// missing (first run). A present-but-unparseable file is an error rather than a
/// silent overwrite — we never clobber config we can't understand.
fn read_json_object(path: &PathBuf) -> Result<serde_json::Value, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| format!("{} is not valid JSON: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(serde_json::Value::Object(serde_json::Map::new()))
        }
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

/// Write pretty JSON (Claude Code's settings stay hand-editable), creating
/// `~/.claude` if needed.
fn write_json_pretty(path: &PathBuf, value: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| format!("could not serialize settings: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Our single hook spec: a fire-and-forget `http` POST to `url`. `async: true`
/// makes Claude Code dispatch the request without blocking the turn (it never waits
/// on Whence's receiver), and the short `timeout` bounds the dispatch so a stalled
/// or absent listener can't hang the hook.
fn whence_hook(url: &str) -> serde_json::Value {
    serde_json::json!({ "type": "http", "url": url, "timeout": HOOK_TIMEOUT_SECS, "async": true })
}

/// Merge Whence's `http` hooks into an existing settings object, idempotently.
///
/// For each event in [`HOOK_EVENTS`], ensures `hooks.<Event>` is an array holding
/// a matcher group whose `hooks` list contains our `{type:"http", url}` entry —
/// adding it only if absent (keyed by `url`). Every other key, and any hooks the
/// user configured themselves, are left untouched. Pure — unit-tested.
fn merge_hook_config(mut root: serde_json::Value, url: &str) -> serde_json::Value {
    let obj = ensure_object(&mut root);
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(hooks_obj) = hooks.as_object_mut() else {
        return root; // `hooks` is some non-object the user owns; don't fight it.
    };

    for event in HOOK_EVENTS {
        let groups = hooks_obj
            .entry(event.to_string())
            .or_insert_with(|| serde_json::Value::Array(vec![]));
        let Some(arr) = groups.as_array_mut() else { continue };

        if hook_present(arr, url) {
            continue; // already installed — idempotent.
        }
        arr.push(serde_json::json!({
            "matcher": "",
            "hooks": [whence_hook(url)],
        }));
    }
    root
}

/// Remove every Whence `http` hook (matched by `url`) from a settings object,
/// pruning now-empty matcher groups and empty event arrays so an uninstall
/// round-trips back to the pre-install shape. Pure — unit-tested.
fn remove_hook_config(mut root: serde_json::Value, url: &str) -> serde_json::Value {
    let Some(obj) = root.as_object_mut() else { return root };
    let Some(hooks_obj) = obj.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return root;
    };

    for event in HOOK_EVENTS {
        let Some(groups) = hooks_obj.get_mut(event).and_then(|g| g.as_array_mut()) else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                list.retain(|h| !is_whence_hook(h, url));
            }
        }
        // Drop matcher groups left with no hooks, then the event key if now empty.
        groups.retain(|g| {
            g.get("hooks")
                .and_then(|h| h.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(true)
        });
    }
    hooks_obj.retain(|_, v| !v.as_array().map(|a| a.is_empty()).unwrap_or(false));
    if hooks_obj.is_empty() {
        obj.remove("hooks");
    }
    root
}

/// True if `arr` (an event's matcher groups) already contains our hook for `url`.
fn hook_present(arr: &[serde_json::Value], url: &str) -> bool {
    arr.iter().any(|group| {
        group
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|list| list.iter().any(|h| is_whence_hook(h, url)))
            .unwrap_or(false)
    })
}

/// A hook entry is ours iff it's an `http` hook pointed at our `url`.
fn is_whence_hook(h: &serde_json::Value, url: &str) -> bool {
    h.get("type").and_then(|t| t.as_str()) == Some("http")
        && h.get("url").and_then(|u| u.as_str()) == Some(url)
}

/// Coerce a `Value` to an object map, replacing a non-object with an empty one.
fn ensure_object(root: &mut serde_json::Value) -> &mut serde_json::Map<String, serde_json::Value> {
    if !root.is_object() {
        *root = serde_json::Value::Object(serde_json::Map::new());
    }
    root.as_object_mut().expect("just set to object")
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "http://127.0.0.1:18450/hook";

    #[test]
    fn merge_adds_all_events_and_preserves_unrelated_keys() {
        let existing = serde_json::json!({
            "model": "claude-opus-4-8",
            "hooks": {
                // A user's own hook on an event we also use — must survive.
                "Stop": [{ "matcher": "", "hooks": [{ "type": "command", "command": "echo hi" }] }]
            }
        });
        let merged = merge_hook_config(existing, URL);

        // Unrelated top-level key untouched.
        assert_eq!(merged["model"], "claude-opus-4-8");

        // All five events present and carrying our hook.
        for event in HOOK_EVENTS {
            let arr = merged["hooks"][event].as_array().unwrap();
            assert!(hook_present(arr, URL), "{event} missing whence hook");
        }
        // The user's pre-existing Stop command hook is still there.
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        let has_user_cmd = stop.iter().any(|g| {
            g["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|h| h["type"] == "command")
        });
        assert!(has_user_cmd, "user's command hook was clobbered");
    }

    #[test]
    fn installed_hook_is_fire_and_forget() {
        let merged = merge_hook_config(serde_json::json!({}), URL);
        let hook = merged["hooks"]["Stop"][0]["hooks"][0].clone();
        assert_eq!(hook["type"], "http");
        assert_eq!(hook["url"], URL);
        assert_eq!(hook["async"], true);
        assert_eq!(hook["timeout"], 5);
    }

    #[test]
    fn merge_is_idempotent() {
        let once = merge_hook_config(serde_json::json!({}), URL);
        let twice = merge_hook_config(once.clone(), URL);
        assert_eq!(once, twice);
        // Exactly one whence hook per event, not duplicated.
        for event in HOOK_EVENTS {
            let count: usize = twice["hooks"][event]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                .filter(|h| is_whence_hook(h, URL))
                .count();
            assert_eq!(count, 1, "{event} has {count} whence hooks");
        }
    }

    #[test]
    fn uninstall_round_trips() {
        let original = serde_json::json!({
            "model": "claude-opus-4-8",
            "hooks": {
                "Stop": [{ "matcher": "", "hooks": [{ "type": "command", "command": "echo hi" }] }]
            }
        });
        let installed = merge_hook_config(original.clone(), URL);
        let removed = remove_hook_config(installed, URL);
        assert_eq!(removed, original, "uninstall did not restore the pre-install shape");
    }

    #[test]
    fn uninstall_from_empty_drops_hooks_key() {
        let installed = merge_hook_config(serde_json::json!({ "model": "x" }), URL);
        let removed = remove_hook_config(installed, URL);
        // No user hooks remained, so the whole `hooks` key is pruned.
        assert_eq!(removed, serde_json::json!({ "model": "x" }));
    }

    #[test]
    fn merge_into_non_object_root_resets_it() {
        // A malformed root (array) becomes a clean object with our hooks.
        let merged = merge_hook_config(serde_json::json!([1, 2, 3]), URL);
        assert!(merged.is_object());
        assert!(hook_present(merged["hooks"]["Stop"].as_array().unwrap(), URL));
    }
}
