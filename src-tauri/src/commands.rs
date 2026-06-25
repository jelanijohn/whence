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
use crate::settings::{self, Settings};

/// Managed app state, shared across commands and the core task.
pub struct AppState {
    pub snapshot: SharedSnapshot,
    pub data_dir: PathBuf,
    pub settings: Arc<Mutex<Settings>>,
}

/// Current focus snapshot (project + status + open-block start).
#[tauri::command]
pub fn get_focus_state(state: State<AppState>) -> FocusSnapshot {
    state
        .snapshot
        .lock()
        .map(|g| g.clone())
        .unwrap_or(FocusSnapshot {
            project: None,
            status: crate::engine::segment::Status::Idle,
            block_start: None,
        })
}

/// Today's closed focus blocks, oldest first — drives the expanded timeline.
#[tauri::command]
pub fn get_today_blocks(state: State<AppState>) -> Result<Vec<FocusBlock>, String> {
    let path = timeline::timeline_path(&state.data_dir);
    timeline::read_day(&path, chrono::Utc::now().timestamp())
        .map_err(|e| format!("could not read timeline: {e}"))
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
