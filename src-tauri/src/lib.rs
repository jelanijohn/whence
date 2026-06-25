//! Whence — ambient focus widget + local attribution sensor.
//!
//! `run()` wires the three plugins, manages shared state, and spawns the core
//! task (transcript watcher → segmenter → outputs). Decision logic lives in
//! `engine::segment` (pure, fixture-tested); orchestration lives in `core`.

mod adapters;
mod commands;
mod engine;
mod orchestrator;
mod neuroskill;
mod settings;

use std::sync::{Arc, Mutex};

use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;

use crate::commands::AppState;
use crate::engine::segment::{FocusSnapshot, Status};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // single-instance MUST be registered first (Tauri requirement): a sensor
        // must not run twice and double-write NeuroSkill labels. A second launch
        // just surfaces the existing widget and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        // The widget remembers its position across launches.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        // Launch-at-login — opt-in only (settings toggle, default OFF). Registered
        // here so the capability exists; nothing is enabled until the user asks.
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .expect("no app data dir");
            std::fs::create_dir_all(&data_dir).ok();

            let loaded = settings::load(&data_dir);
            let shared: orchestrator::SharedSnapshot = Arc::new(Mutex::new(FocusSnapshot {
                project: None,
                status: Status::Idle,
                block_start: None,
            }));
            let settings_state = Arc::new(Mutex::new(loaded.clone()));

            app.manage(AppState {
                snapshot: shared.clone(),
                data_dir: data_dir.clone(),
                settings: settings_state,
            });

            // Spawn the core. The transcript watcher handle is held in-scope for
            // the task's lifetime (dropping it stops watching).
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let aliases = loaded.project_aliases.clone();
                let _watcher = match adapters::claude_code::watch(tx, aliases) {
                    Ok(w) => w,
                    Err(e) => {
                        eprintln!("whence: transcript watcher failed to start: {e}");
                        return;
                    }
                };
                orchestrator::run(handle, rx, shared, data_dir, loaded).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_focus_state,
            commands::get_today_blocks,
            commands::get_settings,
            commands::set_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Whence");
}
