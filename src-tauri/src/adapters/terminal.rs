//! Terminal cwd adapter (optional, v1.5).
//!
//! A shell hook emits the working directory whenever it changes (or on each
//! prompt), giving a **low-confidence project hint** — useful as a corroborator /
//! tiebreaker, never a primary signal (§5.3). It rides an action you already take
//! (`cd`, running a command), so it honors the piggyback principle; it reads only
//! *your own* cwd, never window chrome.
//!
//! **Why corroboration, not attribution.** A cwd is weaker than a Claude Code
//! transcript: a shell can sit in a repo you're not actively working, and a bare
//! `cd` is not focus (treating it as such is the OS-scraping flavor §1 bans). So
//! every event carries a `confidence` below the engine's corroborator cutoff (§7) and
//! the engine never lets it *originate* a focus — it only reinforces the current
//! block when the project matches (`segment.rs`). Its real value: keeping a block
//! alive while you work in the terminal on the focused project with no AI activity.
//!
//! **Transport.** A loopback receiver, mirroring the Claude hooks receiver
//! (`hooks.rs`): a one-line shell hook POSTs `{"cwd": "<dir>"}` to
//! `http://127.0.0.1:18451/cwd` on directory change. Opt-in (`terminal_enabled`,
//! default off) and unauthenticated by design (loopback only, principle #4). The
//! shell snippet is the user's to add — Whence never edits shell rc files silently.
//! E.g. for zsh: `chpwd() { curl -sm1 -d "{\"cwd\":\"$PWD\"}" 127.0.0.1:18451/cwd >/dev/null 2>&1 }`.
//!
//! Two layers, mirroring `engine::segment`'s purity ethos:
//!   * [`cwd_to_event`] — **pure, fixture-tested**: payload → `WorkEvent` (or
//!     `None` when there's no usable cwd). The clock comes in as a parameter.
//!   * [`serve`] — the thin impure shell: bind `tiny_http`, read bodies, map, send.

use std::collections::HashMap;

use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use super::{Surface, WorkEvent, WorkKind};

/// Corroboration confidence for a cwd hint — deliberately below the engine's
/// `corroborator_confidence_cutoff` (~0.6, §7) so it's treated as reinforcing, not
/// originating. Matches the spec's temporal-correlation weight (§6, ~0.4).
const CWD_CONFIDENCE: f64 = 0.4;

/// The shell-hook payload: just the working directory. Forward-compatible (unknown
/// fields ignored), like the Claude hook payload.
#[derive(Debug, Clone, Deserialize)]
pub struct CwdPayload {
    /// The shell's current working directory when the hook fired.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Map a cwd payload to a corroborating `WorkEvent`, or `None` when there's no
/// usable directory. `now_rfc3339` is passed in so this stays pure (no clock read)
/// and unit-testable. `aliases` is the same transcript-dir-name → slug map the
/// other Claude Code surfaces use, so terminal attribution lands on the same slug.
pub fn cwd_to_event(
    p: &CwdPayload,
    aliases: &HashMap<String, String>,
    now_rfc3339: &str,
) -> Option<WorkEvent> {
    let cwd = p.cwd.as_deref()?;
    let project = resolve_slug(cwd, aliases)?;
    Some(WorkEvent {
        ts: now_rfc3339.to_string(),
        surface: Surface::Terminal,
        project: Some(project),
        kind: WorkKind::Active,
        confidence: CWD_CONFIDENCE,
        detail: Some(format!("cwd: {cwd}")),
    })
}

/// Resolve a project slug from a raw cwd:
///   1. **Alias override** keyed by the cwd encoded to Claude Code's transcript
///      dir-name form (`/`→`-`), so a user's existing alias governs terminal
///      attribution too (the slug is the cross-surface join key).
///   2. **Basename** of the cwd — the lossless trailing path segment. Unlike the
///      transcript dir name (whose `/`→`-` encoding makes `one-domino-square`
///      ambiguous), a raw cwd keeps the real final segment, so this is reliable.
fn resolve_slug(cwd: &str, aliases: &HashMap<String, String>) -> Option<String> {
    if let Some(slug) = aliases.get(&encode_dir_name(cwd)) {
        return Some(slug.clone());
    }
    basename(cwd)
}

/// Encode a cwd to the transcript dir-name form Claude Code uses as the alias key
/// (`/root/Projects/waid` → `-root-Projects-waid`), so terminal events resolve
/// against the same alias map. Both path separators map to `-`.
fn encode_dir_name(cwd: &str) -> String {
    cwd.replace(['/', '\\'], "-")
}

/// Trailing path segment of a directory, split on both separators so a Windows-
/// style cwd basenames correctly (matches the hooks adapter).
fn basename(cwd: &str) -> Option<String> {
    let base = cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next()?;
    (!base.is_empty()).then(|| base.to_string())
}

/// Bind the loopback cwd-hint endpoint and serve forever on a dedicated thread,
/// emitting corroborating `WorkEvent`s on `tx`. Returns once the socket is bound
/// (an error means the bind failed — e.g. the port is taken); the serving loop
/// then runs for the process lifetime. Mirrors [`super::hooks::serve`]: accept
/// fast, never block, drop malformed payloads rather than rejecting them.
pub fn serve(
    tx: UnboundedSender<WorkEvent>,
    aliases: HashMap<String, String>,
    addr: &str,
) -> Result<(), String> {
    let server = tiny_http::Server::http(addr)
        .map_err(|e| format!("could not bind terminal cwd receiver on {addr}: {e}"))?;

    std::thread::Builder::new()
        .name("whence-terminal".into())
        .spawn(move || {
            for mut req in server.incoming_requests() {
                let mut body = String::new();
                if req.as_reader().read_to_string(&mut body).is_ok() {
                    if let Ok(payload) = serde_json::from_str::<CwdPayload>(&body) {
                        let now = chrono::Utc::now().to_rfc3339();
                        if let Some(ev) = cwd_to_event(&payload, &aliases, &now) {
                            // Unbounded send never blocks; an error only means the
                            // core task is gone (shutdown), so dropping is correct.
                            let _ = tx.send(ev);
                        }
                    }
                }
                let _ = req.respond(tiny_http::Response::empty(200));
            }
        })
        .map_err(|e| format!("could not spawn terminal cwd receiver thread: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-06-26T12:00:00Z";

    fn no_aliases() -> HashMap<String, String> {
        HashMap::new()
    }

    fn payload(cwd: &str) -> CwdPayload {
        CwdPayload { cwd: Some(cwd.into()) }
    }

    #[test]
    fn cwd_becomes_corroborating_active_event() {
        let ev = cwd_to_event(&payload("/root/Projects/whence"), &no_aliases(), NOW).unwrap();
        assert_eq!(ev.surface, Surface::Terminal);
        assert_eq!(ev.kind, WorkKind::Active);
        assert_eq!(ev.project.as_deref(), Some("whence"));
        assert_eq!(ev.ts, NOW);
        // Low confidence (below the engine's corroborator cutoff) → the engine treats
        // it as corroboration, never origination.
        assert_eq!(ev.confidence, CWD_CONFIDENCE);
        assert_eq!(ev.detail.as_deref(), Some("cwd: /root/Projects/whence"));
    }

    #[test]
    fn basename_is_lossless_unlike_transcript_dir() {
        // A raw cwd keeps the real final segment, so the hyphenated project that the
        // transcript dir-name encoding loses (`-root-one-domino-square` → "square")
        // resolves correctly here without an alias.
        let ev = cwd_to_event(&payload("/root/one-domino-square"), &no_aliases(), NOW).unwrap();
        assert_eq!(ev.project.as_deref(), Some("one-domino-square"));
    }

    #[test]
    fn alias_keyed_by_encoded_cwd_wins() {
        // The same alias map the transcript watcher uses (keyed by dir-name) governs
        // terminal attribution: `/root/Projects/glue-mac` encodes to the alias key.
        let mut aliases = HashMap::new();
        aliases.insert("-root-Projects-glue-mac".to_string(), "glue-mac".to_string());
        let ev = cwd_to_event(&payload("/root/Projects/glue-mac"), &aliases, NOW).unwrap();
        assert_eq!(ev.project.as_deref(), Some("glue-mac"));
    }

    #[test]
    fn trailing_slash_and_missing_cwd_handled() {
        // Trailing slash doesn't swallow the segment.
        let ev = cwd_to_event(&payload("/root/Projects/waid/"), &no_aliases(), NOW).unwrap();
        assert_eq!(ev.project.as_deref(), Some("waid"));
        // No cwd → no event (nothing to attribute).
        assert!(cwd_to_event(&CwdPayload { cwd: None }, &no_aliases(), NOW).is_none());
        // Root / empty basename → no event, not an empty slug.
        assert!(cwd_to_event(&payload("/"), &no_aliases(), NOW).is_none());
    }

    #[test]
    fn payload_tolerates_unknown_fields() {
        let json = r#"{"cwd":"/root/Projects/whence","shell":"zsh","extra":1}"#;
        let p: CwdPayload = serde_json::from_str(json).unwrap();
        assert_eq!(cwd_to_event(&p, &no_aliases(), NOW).unwrap().project.as_deref(), Some("whence"));
    }
}
