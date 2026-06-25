//! Orchestration: wire adapters → segmenter → outputs. This is the only place
//! that's allowed to be impure around the pure engine — it reads the clock, does
//! I/O (timeline append), fires the NeuroSkill label, and pushes snapshots to the
//! widget. The decision logic all lives in `engine::segment`.

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::adapters::WorkEvent;
use crate::engine::segment::{Effect, FocusSnapshot, Segmenter};
use crate::engine::timeline;
use crate::neuroskill;
use crate::settings::Settings;

/// The event Whence emits to the widget on every focus/status change.
pub const FOCUS_EVENT: &str = "whence://focus";

/// Shared, command-readable current snapshot. The core task is the sole writer.
pub type SharedSnapshot = Arc<Mutex<FocusSnapshot>>;

/// Run the core loop: consume `WorkEvent`s, drive the segmenter, and fan the
/// effects out to the timeline, NeuroSkill, and the widget. Runs until `rx`
/// closes (app shutdown). `now_secs` is injected so tests can drive it; in
/// production it's the wall clock.
pub async fn run(
    app: AppHandle,
    mut rx: UnboundedReceiver<WorkEvent>,
    shared: SharedSnapshot,
    data_dir: std::path::PathBuf,
    settings: Settings,
) {
    let mut seg = Segmenter::new(settings.segment_config());
    let timeline_path = timeline::timeline_path(&data_dir);
    let mut idle_check = tokio::time::interval(std::time::Duration::from_secs(30));

    loop {
        let effects = tokio::select! {
            maybe = rx.recv() => match maybe {
                Some(ev) => seg.ingest(&ev),
                None => break, // channel closed → shutting down
            },
            _ = idle_check.tick() => seg.tick(now_secs()),
        };

        if effects.is_empty() {
            continue;
        }

        for effect in &effects {
            match effect {
                Effect::BlockOpened { project, .. } => {
                    if settings.neuroskill_enabled {
                        fire_label(&settings, neuroskill::start_label(project));
                    }
                }
                Effect::BlockClosed(block) => {
                    if let Err(e) = timeline::append_block(&timeline_path, block) {
                        eprintln!("whence: timeline append failed: {e}");
                    }
                    if settings.neuroskill_enabled {
                        fire_label(&settings, neuroskill::end_label(&block.project));
                    }
                }
                Effect::StatusChanged(_) => { /* snapshot push below covers it */ }
            }
        }

        // The engine snapshot is authoritative for the widget.
        let snap = seg.snapshot();
        if let Ok(mut guard) = shared.lock() {
            *guard = snap.clone();
        }
        let _ = app.emit(FOCUS_EVENT, snap);
    }
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Fire one NeuroSkill label, best-effort, off the hot path. A failure (daemon
/// down, wrong token) is logged and never blocks focus tracking. The endpoint and
/// token path honor settings overrides; the token is resolved at call time so a
/// rotated token (and WSL2 Windows-host discovery) is picked up without restart.
fn fire_label(settings: &Settings, text: String) {
    let base = settings
        .neuroskill_endpoint
        .clone()
        .unwrap_or_else(|| neuroskill::DEFAULT_ENDPOINT.to_string());
    let token = neuroskill::client::load_token(settings.neuroskill_token_path.as_deref());
    tokio::spawn(async move {
        if let Err(e) = neuroskill::client::fire_label(&base, token.as_deref(), &text).await {
            eprintln!("whence: NeuroSkill label '{text}' not written: {e}");
        }
    });
}
