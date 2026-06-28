//! Ollama adapter (liveness / status, v1.5).
//!
//! Ollama exposes a local REST API at `http://localhost:11434`. Polling `/api/ps`
//! reports the models currently loaded in memory.
//!
//! **Honest limitation (§5.2 / open decision §14.5 — resolved to liveness-only):**
//! Ollama tells you *local inference is happening*, not *which project it's for*.
//! It is a **liveness/status signal**, not an attribution source — it must never
//! *originate* a focus switch and never *assert idle* (the absence of inference
//! says nothing about whether you're working). So every event it emits carries
//! `project: None` and a low confidence: the engine flips the widget to *active*
//! but opens no block (a `None`-project focus event is dropped after the status
//! update — see `segment.rs`). Temporal-correlation attribution (tagging Ollama
//! activity with the *currently-focused* project) needs engine state the adapter
//! doesn't have; it stays deferred.
//!
//! **Loaded ≠ inferring.** `/api/ps` lists models that are merely *warm* — a model
//! lingers in memory ~5 minutes after its last request (`expires_at`). Treating
//! "a model is loaded" as "inference is happening" would light the widget for the
//! whole keep-alive window. Instead [`detect_activity`] watches `expires_at`
//! *advance* between polls: a bumped keep-alive means a fresh request was served
//! since the last look. That decision is **pure and fixture-tested**; the poll
//! loop is the thin impure shell.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use super::{Surface, WorkEvent, WorkKind};

/// How often to poll `/api/ps`. Frequent enough to catch a served request inside
/// its keep-alive window, cheap enough to ignore (a localhost GET).
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Per-request timeout, so a wedged daemon can't stall the poll loop.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Liveness confidence — deliberately low. It never feeds block confidence (these
/// events carry no project), but it documents the signal's weight (§6: cwd-derived
/// ~1.0, temporal-correlation ~0.4).
const LIVENESS_CONFIDENCE: f64 = 0.4;

/// The `/api/ps` response — only the fields we read; unknown fields ignored.
#[derive(Debug, Default, Deserialize)]
struct PsResponse {
    #[serde(default)]
    models: Vec<PsModel>,
}

#[derive(Debug, Deserialize)]
struct PsModel {
    #[serde(default)]
    name: String,
    /// When the model will unload. Bumped forward on every request, so a change
    /// between polls is the fingerprint of fresh inference.
    #[serde(default)]
    expires_at: Option<String>,
}

/// Decide whether Ollama is *actively* inferring by diffing two `/api/ps`
/// snapshots, and return the new snapshot (model name → `expires_at`) to carry
/// forward. Inference is active when a model **newly appeared** (loaded for a fresh
/// request) or its **`expires_at` advanced** (keep-alive bumped by a request). A
/// model whose `expires_at` is unchanged is merely warming down — not activity.
fn detect_activity(
    prev: &HashMap<String, String>,
    resp: &PsResponse,
) -> (bool, HashMap<String, String>) {
    let mut cur = HashMap::new();
    let mut active = false;
    for m in &resp.models {
        let exp = m.expires_at.clone().unwrap_or_default();
        match prev.get(&m.name) {
            None => active = true,                       // newly loaded → fresh request
            Some(prev_exp) if *prev_exp != exp => active = true, // keep-alive bumped
            _ => {}                                      // loaded but idle → not activity
        }
        cur.insert(m.name.clone(), exp);
    }
    (active, cur)
}

/// A liveness `WorkEvent`: `active`, unattributed, low-confidence. `detail` names
/// the running models for the timeline/debugging.
fn liveness_event(resp: &PsResponse) -> WorkEvent {
    let models: Vec<&str> = resp.models.iter().map(|m| m.name.as_str()).collect();
    WorkEvent {
        ts: chrono::Utc::now().to_rfc3339(),
        surface: Surface::Ollama,
        project: None, // never attributes — liveness only
        source: None,  // unattributed → never becomes a source row
        source_label: None,
        kind: WorkKind::Active,
        confidence: LIVENESS_CONFIDENCE,
        detail: (!models.is_empty()).then(|| format!("ollama: {}", models.join(", "))),
    }
}

/// Poll Ollama's `/api/ps` on an interval, emitting a low-confidence `Active`
/// liveness event on `tx` whenever fresh inference is detected. Runs until the
/// receiver is gone (shutdown). Connection errors (Ollama not installed/running)
/// are swallowed — they just mean "no signal", not a failure.
///
/// The first successful poll only **seeds** the baseline (every loaded model would
/// otherwise look "newly appeared" and fire a spurious event); detection starts on
/// the second poll.
pub async fn poll(tx: UnboundedSender<WorkEvent>, ps_url: String) {
    let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("whence: ollama poll disabled (client build failed): {e}");
            return;
        }
    };

    let mut interval = tokio::time::interval(POLL_INTERVAL);
    let mut prev: Option<HashMap<String, String>> = None;

    loop {
        interval.tick().await;

        let Ok(resp) = client.get(&ps_url).send().await else {
            continue; // Ollama not reachable — no signal, try again next tick.
        };
        let Ok(ps) = resp.json::<PsResponse>().await else {
            continue; // unexpected body — skip, don't crash the loop.
        };

        let cur = match &prev {
            // Steady state: emit only when this poll shows fresh activity.
            Some(p) => {
                let (active, snapshot) = detect_activity(p, &ps);
                if active && tx.send(liveness_event(&ps)).is_err() {
                    return; // core task gone → shut the loop down.
                }
                snapshot
            }
            // First poll: seed the baseline without emitting.
            None => detect_activity(&HashMap::new(), &ps).1,
        };
        prev = Some(cur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(models: &[(&str, &str)]) -> PsResponse {
        PsResponse {
            models: models
                .iter()
                .map(|(n, e)| PsModel {
                    name: n.to_string(),
                    expires_at: Some(e.to_string()),
                })
                .collect(),
        }
    }

    fn snapshot(models: &[(&str, &str)]) -> HashMap<String, String> {
        models.iter().map(|(n, e)| (n.to_string(), e.to_string())).collect()
    }

    #[test]
    fn newly_loaded_model_is_activity() {
        let prev = HashMap::new();
        let (active, cur) = detect_activity(&prev, &ps(&[("llama3", "2026-06-26T12:05:00Z")]));
        assert!(active);
        assert_eq!(cur.get("llama3").map(String::as_str), Some("2026-06-26T12:05:00Z"));
    }

    #[test]
    fn advanced_expiry_is_activity() {
        // Same model, keep-alive bumped forward → a request was just served.
        let prev = snapshot(&[("llama3", "2026-06-26T12:05:00Z")]);
        let (active, _) = detect_activity(&prev, &ps(&[("llama3", "2026-06-26T12:10:00Z")]));
        assert!(active);
    }

    #[test]
    fn unchanged_expiry_is_not_activity() {
        // Model loaded but idle (counting down to unload) → not activity.
        let prev = snapshot(&[("llama3", "2026-06-26T12:05:00Z")]);
        let (active, cur) = detect_activity(&prev, &ps(&[("llama3", "2026-06-26T12:05:00Z")]));
        assert!(!active);
        assert_eq!(cur.len(), 1);
    }

    #[test]
    fn unloaded_model_is_not_activity() {
        // A model that disappeared (unloaded) is not current activity.
        let prev = snapshot(&[("llama3", "2026-06-26T12:05:00Z")]);
        let (active, cur) = detect_activity(&prev, &ps(&[]));
        assert!(!active);
        assert!(cur.is_empty());
    }

    #[test]
    fn one_active_model_among_idle_ones_triggers() {
        let prev = snapshot(&[
            ("llama3", "2026-06-26T12:05:00Z"),
            ("qwen", "2026-06-26T12:05:00Z"),
        ]);
        // qwen unchanged, llama3 bumped → active.
        let (active, _) = detect_activity(
            &prev,
            &ps(&[
                ("llama3", "2026-06-26T12:11:00Z"),
                ("qwen", "2026-06-26T12:05:00Z"),
            ]),
        );
        assert!(active);
    }

    #[test]
    fn liveness_event_is_unattributed_active() {
        let ev = liveness_event(&ps(&[("llama3", "2026-06-26T12:05:00Z")]));
        assert_eq!(ev.surface, Surface::Ollama);
        assert_eq!(ev.kind, WorkKind::Active);
        assert_eq!(ev.project, None); // never originates a focus switch
        assert_eq!(ev.confidence, LIVENESS_CONFIDENCE);
        assert_eq!(ev.detail.as_deref(), Some("ollama: llama3"));
    }

    #[test]
    fn ps_response_tolerates_unknown_fields() {
        let json = r#"{"models":[{"name":"llama3","expires_at":"2026-06-26T12:05:00Z","size":42}],"extra":1}"#;
        let ps: PsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(ps.models.len(), 1);
        assert_eq!(ps.models[0].name, "llama3");
    }
}
