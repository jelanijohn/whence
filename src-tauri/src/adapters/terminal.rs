//! Terminal cwd adapter (optional, v1.5 — **stub**).
//!
//! A shell hook (or watching a shell's cwd) can emit a low-confidence project
//! hint — useful as a tiebreaker / corroborator, never a primary signal. Left
//! inert until the core engine is calibrated; documented here so the adapter slot
//! exists and the `WorkEvent` contract is the only thing a future implementation
//! needs to satisfy.

use tokio::sync::mpsc::UnboundedSender;

use super::WorkEvent;

/// Placeholder for the v1.5 cwd-hint source. Emits nothing yet.
pub async fn run(_tx: UnboundedSender<WorkEvent>) {
    // TODO(v1.5): receive cwd hints (shell hook → localhost, or a watched file)
    // and emit low-confidence (~0.4) project hints as corroborating evidence.
}
