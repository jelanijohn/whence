//! NeuroSkill **connection health** — a periodic, side-effect-free probe that tells
//! the widget whether the label write path is actually live.
//!
//! Mirrors the Ollama poll loop's shape (`adapters/ollama.rs`): an independent task,
//! spawned in `lib.rs`, that never blocks the core. The difference from the write
//! path is the whole point — this **never mutates** NeuroSkill. It sends a benign
//! no-op command purely to exercise reachability + bearer auth, so the header
//! indicator stays honest *between* block boundaries (the write path only touches
//! the daemon on block open/close, which could be minutes apart).
//!
//! Settings are read each tick, so toggling `neuroskill_enabled` or changing the
//! endpoint/token reflects in the indicator without a restart — the same call-time
//! resolution `fire_label` uses for the token.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::neuroskill::{self, client};
use crate::settings::Settings;

/// How the widget paints the NeuroSkill connection. The frontend mirror is the
/// `NeuroskillStatus` union in `types.ts` — keep the snake_case serde reps in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NeuroskillStatus {
    /// Label writing is turned off (`neuroskill_enabled = false`) — we don't probe.
    Disabled,
    /// Daemon reachable and the bearer token accepted.
    Connected,
    /// Daemon reachable but it rejected auth (token missing/wrong) — an HTTP 401.
    Unauthorized,
    /// Daemon not reachable (down, or wrong endpoint).
    Unreachable,
    /// Not probed yet — the initial state before the first tick lands.
    #[default]
    Unknown,
}

/// Probe cadence. NeuroSkill is local, so a frequent cheap check is fine; this keeps
/// the indicator fresh without being chatty against the daemon.
const PROBE_INTERVAL: Duration = Duration::from_secs(15);

/// Per-probe timeout, so a wedged daemon can't stall the loop (mirrors Ollama).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Event name for connection-status pushes — distinct from the focus event so the
/// widget subscribes independently (a connection change isn't a focus change).
pub const STATUS_EVENT: &str = "whence://neuroskill";

/// Shared, command-readable connection status. The probe loop is the sole writer;
/// `get_neuroskill_status` reads it for the widget's first paint.
pub type SharedStatus = Arc<Mutex<NeuroskillStatus>>;

/// Periodically probe the NeuroSkill daemon and push the connection status to the
/// widget. Reads settings each tick (runtime toggle/endpoint changes take effect
/// without a restart). Updates the shared state every tick but only *emits* on a
/// change, to avoid event spam. Runs for the life of the app.
pub async fn watch(app: AppHandle, settings: Arc<Mutex<Settings>>, shared: SharedStatus) {
    let mut interval = tokio::time::interval(PROBE_INTERVAL);
    let mut last: Option<NeuroskillStatus> = None;

    loop {
        interval.tick().await;

        // Snapshot just the fields we need, then drop the lock before any await.
        let (enabled, base, token_path) = {
            let Ok(g) = settings.lock() else { continue };
            (
                g.neuroskill_enabled,
                g.neuroskill_endpoint
                    .clone()
                    .unwrap_or_else(|| neuroskill::DEFAULT_ENDPOINT.to_string()),
                g.neuroskill_token_path.clone(),
            )
        };

        let status = if !enabled {
            NeuroskillStatus::Disabled
        } else {
            // Resolve the token at probe time (rotation / WSL2 discovery), like the
            // write path. A missing token still probes — the daemon answers 401.
            let token = client::load_token(token_path.as_deref());
            probe(&base, token.as_deref()).await
        };

        if let Ok(mut g) = shared.lock() {
            *g = status;
        }
        if last != Some(status) {
            last = Some(status);
            let _ = app.emit(STATUS_EVENT, status);
        }
    }
}

/// One health probe: POST a benign no-op command to the daemon and classify the
/// outcome. **Never mutates** — the daemon checks the bearer token before it
/// dispatches a command, so even an unrecognized `ping` exercises reachability and
/// auth without writing a label. A send error means the daemon is down; a 401 means
/// the token is missing/wrong; any other response means we got through.
async fn probe(base: &str, token: Option<&str>) -> NeuroskillStatus {
    let url = format!("{}/", base.trim_end_matches('/'));
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(_) => return NeuroskillStatus::Unreachable,
    };

    let mut req = client
        .post(&url)
        .json(&serde_json::json!({ "command": "ping" }));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }

    match req.send().await {
        Err(_) => NeuroskillStatus::Unreachable,
        Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
            NeuroskillStatus::Unauthorized
        }
        Ok(_) => NeuroskillStatus::Connected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The serde reps are a cross-language contract: `types.ts`'s `NeuroskillStatus`
    /// union must match these strings exactly, or the widget can't read the event.
    #[test]
    fn status_serializes_snake_case() {
        let rep = |s: NeuroskillStatus| serde_json::to_string(&s).unwrap();
        assert_eq!(rep(NeuroskillStatus::Disabled), "\"disabled\"");
        assert_eq!(rep(NeuroskillStatus::Connected), "\"connected\"");
        assert_eq!(rep(NeuroskillStatus::Unauthorized), "\"unauthorized\"");
        assert_eq!(rep(NeuroskillStatus::Unreachable), "\"unreachable\"");
        assert_eq!(rep(NeuroskillStatus::Unknown), "\"unknown\"");
    }

    /// The pre-probe default is `Unknown` — the widget hides the indicator until the
    /// first real probe lands, so first paint stays quiet.
    #[test]
    fn default_is_unknown() {
        assert_eq!(NeuroskillStatus::default(), NeuroskillStatus::Unknown);
    }
}
