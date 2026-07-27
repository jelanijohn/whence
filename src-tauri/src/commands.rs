//! Tauri commands — the frontend's only door into the backend. Thin: query the
//! shared focus snapshot, read the JSONL timeline, and get/set settings. The
//! `tauri.ts` binding layer mirrors these one-to-one.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use tokio::sync::mpsc::UnboundedSender;

use crate::adapters::browser::RaiseQueue;
use crate::adapters::{Surface, WorkEvent, WorkKind};
use crate::orchestrator::{SharedSnapshot, TimelineRecord, WidgetSnapshot};
use crate::engine::timeline;
use crate::neuroskill::health::{NeuroskillStatus, SharedStatus};
use crate::settings::{self, Settings};

/// Broadcast on every settings save (mirrors `orchestrator::FOCUS_EVENT`) so the
/// widget realm picks up appearance changes made in the settings window.
pub const SETTINGS_EVENT: &str = "whence://settings";

/// Managed app state, shared across commands and the core task.
pub struct AppState {
    pub snapshot: SharedSnapshot,
    pub data_dir: PathBuf,
    pub settings: Arc<Mutex<Settings>>,
    /// Live NeuroSkill connection status — written by the health probe loop, read
    /// here for the widget's first paint (it then tracks the `whence://neuroskill`
    /// event).
    pub neuroskill: SharedStatus,
    /// The core `WorkEvent` channel — the *only* way a command can influence focus, by
    /// injecting an event the segmenter ingests like any adapter's (`focus_source`).
    pub tx: UnboundedSender<WorkEvent>,
    /// Pending browser-tab raises the extension drains via `GET /raise`.
    pub raise_queue: RaiseQueue,
    /// The live receiver bearer token — shared with the three listener threads, so a
    /// rotation here applies to them without a restart.
    pub receiver_token: crate::auth::SharedToken,
    /// Live context moments — held here so disabling a moment source in Settings
    /// purges its already-captured text immediately (not just hides it until the
    /// block closes).
    pub moments: crate::context::SharedMoments,
    /// Denied (401) receiver requests since launch — the Settings diagnostic.
    pub auth_denials: crate::auth::Denials,
}

/// Current focus snapshot — every live session (each with its own status + timer),
/// plus the focused project's context string when one has resolved. A poisoned
/// lock falls back to "no sessions" (idle) rather than panicking.
#[tauri::command]
pub fn get_focus_state(state: State<AppState>) -> WidgetSnapshot {
    state
        .snapshot
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Manually raise a browser tab and pull its project into focus. The frontend only
/// fires this for browser source rows (the one surface whose tab the extension can
/// raise), passing the project slug and the source's normalized URL.
///
/// Does *both* halves of a click: (1) enqueues the raise for the extension's `GET
/// /raise` poll, and (2) injects a synthetic *you-acted* `Select` event so focus
/// switches immediately (marked `present`) — which drives the NeuroSkill `:start`/
/// `:end` labels and the timeline block through the normal effect path, no special
/// casing. Degrades cleanly: with the extension absent the raise is simply never
/// drained, and focus still switches.
#[tauri::command]
pub fn focus_source(state: State<AppState>, project: String, source: String) -> Result<(), String> {
    crate::adapters::browser::enqueue_raise(&state.raise_queue, source.clone());
    let ev = WorkEvent {
        ts: chrono::Utc::now().to_rfc3339(),
        surface: Surface::Browser,
        project: Some(project),
        source: Some(source.clone()),
        // The row already exists; the engine only sets a source's label on insert, so
        // leaving this `None` preserves the existing `"claude web"`/`"chatgpt web"`.
        source_label: None,
        kind: WorkKind::Select,
        confidence: 1.0,
        detail: Some(format!("manual select: {source}")),
    };
    state.tx.send(ev).map_err(|_| "core task unavailable".to_string())
}

/// Today's closed focus blocks, oldest first — drives the expanded timeline. Each
/// carries its stored context stamp, if the block closed with one.
#[tauri::command]
pub fn get_today_blocks(state: State<AppState>) -> Result<Vec<TimelineRecord>, String> {
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
    let prev = state
        .settings
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    // Reconcile autostart with the OS only when it actually changed, so we don't
    // re-register on every settings save.
    let prev_autostart = prev.autostart;
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

    // A moment source whose effective gate (the same conjunction the receivers
    // capture under) just turned off gets its stored text dropped now — an
    // opt-in privacy gate must release captured content on disable, not retain
    // it hidden until the block closes.
    let gates = [
        (
            crate::context::ContextSource::HookPrompt,
            prev.context_strings && prev.context_hook_prompts,
            settings.context_strings && settings.context_hook_prompts,
        ),
        (
            crate::context::ContextSource::BrowserTitle,
            prev.context_strings && prev.context_browser_titles,
            settings.context_strings && settings.context_browser_titles,
        ),
    ];
    for (source, was_on, is_on) in gates {
        if was_on && !is_on {
            crate::context::purge_source(&state.moments, source);
        }
    }

    // Apply the window flags to the live `main` window. Idempotent, so no
    // change-detection is needed; `set_visible_on_all_workspaces` is best-effort
    // (unsupported on Windows/mobile) and must never block a settings save.
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_always_on_top(settings.always_on_top);
        let _ = win.set_visible_on_all_workspaces(settings.always_present);
    }

    // Broadcast the save so other webview realms (the widget, while settings live
    // in their own window) refresh appearance without a second event channel.
    let _ = app.emit(SETTINGS_EVENT, &settings);

    Ok(settings)
}

/// Open the settings popup, or focus it if already open. Settings live in their
/// own decorated window (label "settings", route /settings) so the widget stays
/// compact. `async` is load-bearing: on Windows, building a webview from a
/// synchronous command deadlocks WebView2 init (wry#583) — the window opens
/// blank and won't even close.
#[tauri::command]
pub async fn open_settings(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(&app, "settings", tauri::WebviewUrl::App("settings".into()))
        .title("Whence Settings")
        .inner_size(400.0, 560.0)
        .min_inner_size(340.0, 420.0)
        .center()
        .build()
        .map_err(|e| format!("could not open settings window: {e}"))?;
    Ok(())
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
///
/// The written URL embeds the receiver bearer token (`/hook/<token>` — `http` hooks
/// can't set headers), so the existing install/uninstall round-trip carries auth
/// with zero extra user steps. Stale Whence entries under the same base (an old
/// token, or the pre-auth bare `/hook`) are pruned first, so re-install after a
/// rotation converges instead of accumulating dead hooks.
#[tauri::command]
pub fn install_claude_hooks(state: State<AppState>) -> Result<(), String> {
    let url = hook_url(&state);
    let path = claude_settings_path(&state)?;
    let existing = read_json_object(&path)?;
    let pruned = remove_hook_config(existing, &hook_base_url(&state));
    let merged = merge_hook_config(pruned, &url);
    write_json_pretty(&path, &merged)
}

/// Remove Whence's `http` hook entries from `~/.claude/settings.json`, leaving all
/// other config intact. Round-trips `install` exactly. Matches by the base URL, so
/// entries carrying any token generation (or none) are all removed.
#[tauri::command]
pub fn uninstall_claude_hooks(state: State<AppState>) -> Result<(), String> {
    let base = hook_base_url(&state);
    let path = claude_settings_path(&state)?;
    let existing = read_json_object(&path)?;
    let pruned = remove_hook_config(existing, &base);
    write_json_pretty(&path, &pruned)
}

/// Receiver auth surfaced to Settings: the live token, where it's stored, and how
/// many unauthenticated requests the receivers have rejected since launch.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiverAuth {
    pub token: String,
    pub token_path: String,
    pub denials: u64,
    /// Set when the operation succeeded but a best-effort follow-up didn't (a
    /// rotation whose installed-hooks rewrite failed). The state above is still
    /// the live truth — the warning tells the user what to do next, without the
    /// UI mistaking a done rotation for a failed one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Current receiver-auth state — drives the Settings section (token display, the
/// terminal snippet, and the denial counter).
#[tauri::command]
pub fn get_receiver_auth(state: State<AppState>) -> ReceiverAuth {
    ReceiverAuth {
        token: crate::auth::read_token(&state.receiver_token).clone(),
        token_path: crate::auth::token_path(&state.data_dir).to_string_lossy().into_owned(),
        denials: state.auth_denials.load(std::sync::atomic::Ordering::Relaxed),
        warning: None,
    }
}

/// Rotate the receiver token: mint + persist a new one and apply it to the live
/// listeners immediately. If Claude Code hooks are installed, their URLs are
/// rewritten to carry the new token in the same pass (best-effort). `Err` means
/// the rotation itself didn't happen (persist failed; the old token is still
/// live everywhere). A rotation that succeeded but couldn't rewrite the hooks
/// returns `Ok` with a `warning` — the new token IS live, and reporting that as
/// an error would leave the UI showing a token the receivers no longer accept.
/// The terminal snippet and extension need the new token pasted — that's the
/// point of a rotation.
#[tauri::command]
pub fn rotate_receiver_token(state: State<AppState>) -> Result<ReceiverAuth, String> {
    let token = crate::auth::rotate(&state.data_dir)
        .map_err(|e| format!("could not persist the new token: {e}"))?;
    // Recover a poisoned lock rather than skip: silently keeping the old token
    // live (while the new one is already on disk) would desync the receivers and
    // make the hook refresh below re-embed the *old* token.
    *state
        .receiver_token
        .write()
        .unwrap_or_else(|e| e.into_inner()) = token;

    // Refresh installed hooks to the new URL. Only rewrite when our hooks are
    // actually present — rotation must not install hooks the user never opted into.
    let refresh = (|| -> Result<(), String> {
        let base = hook_base_url(&state);
        let path = claude_settings_path(&state)?;
        let existing = read_json_object(&path)?;
        if !config_has_whence_hooks(&existing, &base) {
            return Ok(());
        }
        let merged = merge_hook_config(remove_hook_config(existing, &base), &hook_url(&state));
        write_json_pretty(&path, &merged)
    })();

    let mut auth = get_receiver_auth(state);
    if let Err(e) = refresh {
        auth.warning = Some(format!(
            "Token rotated, but the installed Claude hooks could not be updated ({e}) — \
             re-run Install hooks"
        ));
    }
    Ok(auth)
}

/// The receiver URL written into the hook config — `http://<bind>/hook/<token>`,
/// where the bind is the (possibly overridden) loopback address the listener is
/// using and the token is the live receiver bearer token (the `http` hook's only
/// way to carry a credential).
fn hook_url(state: &State<AppState>) -> String {
    let token = crate::auth::read_token(&state.receiver_token);
    format!("{}/{}", hook_base_url(state), *token)
}

/// The token-less hook URL prefix — the identity by which *any* generation of
/// Whence hook entry is recognized for pruning/uninstall.
fn hook_base_url(state: &State<AppState>) -> String {
    let addr = state
        .settings
        .lock()
        .map(|g| g.hook_listen_addr())
        .unwrap_or_else(|_| settings::DEFAULT_HOOK_ADDR.to_string());
    format!("http://{addr}/hook")
}

/// `<claude_dir>/settings.json` — the user-scope config, so the hooks fire for
/// *every* Claude Code session regardless of project. Resolved through the same
/// `claude_dir` discovery the transcript watcher uses (override → native home →
/// WSL distro walk on Windows), so hooks always install into the *same* Claude
/// Code the transcripts come from — including WSL's `~/.claude/settings.json`,
/// written through `\\wsl$\`, when Whence runs natively on the Windows host.
fn claude_settings_path(state: &State<AppState>) -> Result<PathBuf, String> {
    let override_dir = state
        .settings
        .lock()
        .ok()
        .and_then(|g| g.claude_dir.clone());
    let dir = crate::adapters::claude_code::claude_dir(override_dir.as_deref()).ok_or(
        "could not resolve the Claude Code config dir — set the Claude dir override in Settings",
    )?;
    Ok(dir.join("settings.json"))
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

/// Remove every Whence `http` hook (any entry under `base` — with or without an
/// embedded token) from a settings object, pruning now-empty matcher groups and
/// empty event arrays so an uninstall round-trips back to the pre-install shape.
/// Pure — unit-tested.
fn remove_hook_config(mut root: serde_json::Value, base: &str) -> serde_json::Value {
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
                list.retain(|h| !is_whence_hook_under(h, base));
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

/// A hook entry is ours iff it's an `http` hook pointed at exactly our `url`
/// (base + current token) — the idempotence key for merge.
fn is_whence_hook(h: &serde_json::Value, url: &str) -> bool {
    h.get("type").and_then(|t| t.as_str()) == Some("http")
        && h.get("url").and_then(|u| u.as_str()) == Some(url)
}

/// A hook entry is *some generation of* ours iff it's an `http` hook whose URL is
/// `base` itself (the pre-auth shape) or a `base/<anything>` (any token) —
/// segment-bounded, so an unrelated `.../hooked` never matches. The removal key.
fn is_whence_hook_under(h: &serde_json::Value, base: &str) -> bool {
    if h.get("type").and_then(|t| t.as_str()) != Some("http") {
        return false;
    }
    match h.get("url").and_then(|u| u.as_str()) {
        Some(u) => u == base || u.strip_prefix(base).is_some_and(|rest| rest.starts_with('/')),
        None => false,
    }
}

/// Whether a settings object carries any Whence hook entry under `base` — the
/// "were hooks installed?" probe the rotation refresh keys on.
fn config_has_whence_hooks(root: &serde_json::Value, base: &str) -> bool {
    let Some(hooks_obj) = root.get("hooks").and_then(|h| h.as_object()) else {
        return false;
    };
    HOOK_EVENTS.iter().any(|event| {
        hooks_obj
            .get(*event)
            .and_then(|g| g.as_array())
            .map(|groups| {
                groups.iter().any(|group| {
                    group
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .map(|list| list.iter().any(|h| is_whence_hook_under(h, base)))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    })
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

    const BASE: &str = "http://127.0.0.1:18450/hook";

    #[test]
    fn install_after_rotation_converges_to_one_hook() {
        // The install path: prune everything under the base, then merge the current
        // tokened URL — an old-token entry (or the pre-auth bare /hook) never
        // accumulates alongside the new one.
        let old_url = format!("{BASE}/oldtok");
        let new_url = format!("{BASE}/newtok");
        let with_old = merge_hook_config(serde_json::json!({}), &old_url);
        let converged = merge_hook_config(remove_hook_config(with_old, BASE), &new_url);
        for event in HOOK_EVENTS {
            let arr = converged["hooks"][event].as_array().unwrap();
            assert!(hook_present(arr, &new_url), "{event} missing new-token hook");
            assert!(!hook_present(arr, &old_url), "{event} kept stale-token hook");
        }
    }

    #[test]
    fn uninstall_removes_any_token_generation() {
        // Uninstall keys on the base, so a tokened install and a legacy bare-/hook
        // install are both fully removed.
        let tokened = merge_hook_config(serde_json::json!({}), &format!("{BASE}/tok123"));
        assert_eq!(remove_hook_config(tokened, BASE), serde_json::json!({}));
        let legacy = merge_hook_config(serde_json::json!({}), BASE);
        assert_eq!(remove_hook_config(legacy, BASE), serde_json::json!({}));
    }

    #[test]
    fn base_matching_is_segment_bounded() {
        // A user's own http hook at a URL that merely extends the base string is
        // NOT ours and must survive an uninstall.
        let foreign = serde_json::json!({
            "hooks": {
                "Stop": [{ "matcher": "", "hooks": [
                    { "type": "http", "url": "http://127.0.0.1:18450/hooked" }
                ]}]
            }
        });
        assert_eq!(remove_hook_config(foreign.clone(), BASE), foreign);
        assert!(!config_has_whence_hooks(&foreign, BASE));
    }

    #[test]
    fn installed_probe_detects_any_generation() {
        assert!(!config_has_whence_hooks(&serde_json::json!({}), BASE));
        let tokened = merge_hook_config(serde_json::json!({}), &format!("{BASE}/tok"));
        assert!(config_has_whence_hooks(&tokened, BASE));
        let legacy = merge_hook_config(serde_json::json!({}), BASE);
        assert!(config_has_whence_hooks(&legacy, BASE));
    }
}
