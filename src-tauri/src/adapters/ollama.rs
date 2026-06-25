//! Ollama adapter (liveness / status, v1.5 — **stub**).
//!
//! Ollama exposes a local REST API at `http://localhost:11434`. Polling `/api/ps`
//! reports running/loaded models and whether inference is active.
//!
//! **Honest limitation (open decision §14.5):** Ollama tells you *local inference
//! is happening*, not *which project it's for*. It is a **liveness/status signal**,
//! not an attribution source — it must never *originate* a focus switch. The
//! optional enrichment (attribute Ollama activity to the currently-focused project
//! by temporal correlation, at ~0.4 confidence) is left for when the core engine
//! is calibrated; until then this adapter is intentionally inert.

use tokio::sync::mpsc::UnboundedSender;

use super::WorkEvent;

pub const OLLAMA_PS_URL: &str = "http://localhost:11434/api/ps";

/// Placeholder for the v1.5 poll loop. Wired but inert: it does not emit events
/// yet, so it cannot color a block before the temporal-correlation policy lands.
pub async fn poll(_tx: UnboundedSender<WorkEvent>) {
    // TODO(v1.5): poll OLLAMA_PS_URL on an interval; emit a low-confidence
    // `Active`/`Idle` status only — never a project-originating focus switch.
}
