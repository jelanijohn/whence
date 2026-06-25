//! NeuroSkill integration — the **write path** that makes Whence a *producer*.
//!
//! When the focus engine confirms a block on project P, Whence writes session
//! labels into NeuroSkill so its EEG epochs get attributed to P automatically —
//! no manual session-start. This mirrors WAID's proven contract exactly, with one
//! difference confirmed against WAID's live code: the contract is **HTTP**, not a
//! WebSocket. (The spec §8 describes a `ws://127.0.0.1:8375` hand-rolled frame
//! writer — that is stale. WAID's `neuroskill/client.rs` fires the `label` command
//! over a plain `POST /` to the daemon's local HTTP API, bearer-token gated. We
//! lift that.)
//!
//! Read-scope discipline (principle #5): the only thing Whence writes is the
//! label. The optional EEG read-back (`eeg`, behind the `eeg-readback` feature) is
//! strictly read-only, exactly as WAID opens NeuroSkill's SQLite `mode=ro&immutable=1`.
//!
//! Label namespace (resolves open decision §14.4): `Whence:project=<slug>:start|end`.
//! Source-namespaced so it never collides with WAID's `waid:brief=<slug>:…`;
//! downstream readers prefer the manual `waid:` label on conflict (you said so;
//! Whence only inferred).

pub mod client;

#[cfg(feature = "eeg-readback")]
pub mod eeg;

/// Default NeuroSkill daemon HTTP origin (localhost, the daemon's default port) —
/// matches WAID's `DEFAULT_ENDPOINT`.
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:18444";

/// The label namespace for inferred (auto) attribution. Distinct from WAID's
/// manual `waid:brief=` prefix so the two writers never stomp each other.
pub const LABEL_PREFIX: &str = "Whence:project=";

/// Build the `:start` label for a project slug.
pub fn start_label(slug: &str) -> String {
    format!("{LABEL_PREFIX}{slug}:start")
}

/// Build the `:end` label for a project slug.
pub fn end_label(slug: &str) -> String {
    format!("{LABEL_PREFIX}{slug}:end")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_source_namespaced() {
        assert_eq!(start_label("waid"), "Whence:project=waid:start");
        assert_eq!(end_label("waid"), "Whence:project=waid:end");
        // Distinct from WAID's manual prefix — no collision in the shared table.
        assert!(start_label("waid").starts_with("Whence:"));
        assert!(!start_label("waid").starts_with("waid:"));
    }
}
