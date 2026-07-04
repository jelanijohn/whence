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
mod tray;

use std::sync::{Arc, Mutex};

use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;

use crate::commands::AppState;
use crate::engine::segment::FocusSnapshot;

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

            // Apply the persisted window flags once the main window exists, so the
            // setting (not just `tauri.conf.json`) is the single source of truth.
            // `set_visible_on_all_workspaces` is best-effort (no-op on Windows/mobile).
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_always_on_top(loaded.always_on_top);
                let _ = win.set_visible_on_all_workspaces(loaded.always_present);
            }

            // System tray: the only affordance for un-hiding the decorationless
            // widget and the canonical Quit path. Bundle icons exist by now, so
            // `default_window_icon()` is populated.
            tray::init(app.handle())?;

            let shared: orchestrator::SharedSnapshot =
                Arc::new(Mutex::new(FocusSnapshot { projects: Vec::new() }));
            let settings_state = Arc::new(Mutex::new(loaded.clone()));
            let neuroskill_status: neuroskill::health::SharedStatus =
                Arc::new(Mutex::new(neuroskill::health::NeuroskillStatus::default()));

            // The core event channel and the browser raise queue are created here (above
            // `manage`) so a clone of each can live in `AppState`: `focus_source` injects
            // a synthetic event on `tx` and enqueues a tab raise. `rx` is moved into the
            // core task below.
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<adapters::WorkEvent>();
            let raise_queue: adapters::browser::RaiseQueue =
                Arc::new(Mutex::new(std::collections::VecDeque::new()));

            app.manage(AppState {
                snapshot: shared.clone(),
                data_dir: data_dir.clone(),
                settings: settings_state.clone(),
                neuroskill: neuroskill_status.clone(),
                tx: tx.clone(),
                raise_queue: raise_queue.clone(),
            });

            // NeuroSkill connection health: an independent probe loop that keeps the
            // widget's connection indicator honest. Reads settings each tick, so it
            // reflects the `neuroskill_enabled` toggle (and endpoint/token) live.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(neuroskill::health::watch(
                    handle,
                    settings_state,
                    neuroskill_status,
                ));
            }

            // Spawn the core. The transcript watcher handle is held in-scope for
            // the task's lifetime (dropping it stops watching).
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // `tx`/`rx` and `raise_queue` are the hoisted handles (a `tx` clone +
                // the queue already live in `AppState`); this `move` closure owns them.
                let aliases = loaded.project_aliases.clone();

                // Hooks receiver (v1.5): clone the sender *before* `watch` consumes
                // it, then bind the loopback endpoint. Non-fatal on failure — a
                // taken port just means no live `awaiting_input`; transcript-watch
                // still runs (degrade, don't crash). The transcripts root lets the
                // receiver rebase a payload's transcript_path when Claude Code runs
                // in WSL but Whence on the Windows host (different filesystems).
                let hook_addr = loaded.hook_listen_addr();
                let transcripts_root =
                    adapters::claude_code::transcripts_root(loaded.claude_dir.as_deref());
                if let Err(e) = adapters::hooks::serve(
                    tx.clone(),
                    aliases.clone(),
                    &hook_addr,
                    transcripts_root,
                ) {
                    eprintln!("whence: hook receiver not started: {e}");
                }

                // Ollama liveness (v1.5): low-confidence status only, opt-out via
                // settings. Independent task — it never blocks the watcher.
                if loaded.ollama_enabled {
                    let ollama_tx = tx.clone();
                    let ps_url = loaded.ollama_ps_url();
                    tauri::async_runtime::spawn(adapters::ollama::poll(ollama_tx, ps_url));
                }

                // Terminal cwd receiver (v1.5): opt-in corroborating hints from a
                // shell hook. Non-fatal on failure — a taken port just means no cwd
                // corroboration; everything else still runs (degrade, don't crash).
                if loaded.terminal_enabled {
                    let terminal_addr = loaded.terminal_listen_addr();
                    if let Err(e) =
                        adapters::terminal::serve(tx.clone(), aliases.clone(), &terminal_addr)
                    {
                        eprintln!("whence: terminal cwd receiver not started: {e}");
                    }
                }

                // Browser receiver (opt-in, default off): originating-capable browser
                // LLM sessions from the first-party extension. A corrupt mapping file
                // disables it (rather than clobbering the user's hand-edits); a taken
                // port likewise just means no browser attribution — degrade, don't crash.
                if loaded.browser_enabled {
                    let browser_addr = loaded.browser_listen_addr();
                    let map_path = adapters::browser_map::mapping_path(&data_dir);
                    match adapters::browser_map::MappingStore::load(&map_path) {
                        Ok(store) => {
                            if let Err(e) = adapters::browser::serve(
                                tx.clone(),
                                store,
                                &browser_addr,
                                raise_queue.clone(),
                            ) {
                                eprintln!("whence: browser receiver not started: {e}");
                            }
                        }
                        Err(e) => eprintln!("whence: browser receiver not started: {e}"),
                    }
                }

                // The transcript watcher is best-effort like every other adapter: a
                // failed start must NOT kill the engine. Returning here would drop
                // `rx` and silently void every event from the receivers above (the
                // v0.1.0 Windows bug: no $HOME → early return → browser/hook events
                // sent into a closed channel). Hold a successful watcher's handle in
                // an Option so it stays alive for the task's lifetime.
                let _watcher = match adapters::claude_code::watch(
                    tx,
                    aliases,
                    loaded.claude_dir.as_deref(),
                ) {
                    Ok(w) => Some(w),
                    Err(e) => {
                        eprintln!("whence: transcript watcher failed to start: {e}");
                        None
                    }
                };
                orchestrator::run(handle, rx, shared, data_dir, loaded).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_focus_state,
            commands::focus_source,
            commands::get_today_blocks,
            commands::get_focus_intensity,
            commands::get_neuroskill_status,
            commands::get_browser_mapping_path,
            commands::get_settings,
            commands::set_settings,
            commands::install_claude_hooks,
            commands::uninstall_claude_hooks,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Whence");
}
