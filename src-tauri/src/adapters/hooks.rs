//! Claude Code **hooks** receiver (v1.5) — live status fidelity.
//!
//! Transcript-watch (`claude_code.rs`) can tell that Claude Code is *active*, but
//! not whether it's **running** vs. **awaiting your input** — the most valuable
//! status distinction for an ambient widget, and the one transcript mtimes can't
//! cleanly infer. Claude Code lifecycle hooks can: a `Stop` hook fires the instant
//! a turn finishes (you're now on the clock), a `UserPromptSubmit` fires the
//! instant you reply.
//!
//! Whence registers Claude Code's native `http` hooks (opt-in, via
//! `commands::install_claude_hooks`) pointed at a loopback endpoint this module
//! serves. The `http` hook POSTs the event JSON and does **not** block on the
//! response — Claude Code never waits on us.
//!
//! Two layers, mirroring `engine::segment`'s purity ethos:
//!   * [`hook_to_event`] — **pure, fixture-tested**: payload → `WorkEvent` (or
//!     `None` for events we ignore). The clock comes in as a parameter.
//!   * [`serve`] — the thin impure shell: bind `tiny_http`, read bodies, map, send.
//!
//! Status-only events (`Stop`/`Notification`) carry `project: None` — the engine
//! ignores `project` for status (§7), so attribution there is moot. Focus-evidence
//! events (`UserPromptSubmit` → `Prompt`) resolve a slug so the *active* status
//! lands on the right project without waiting for the transcript debounce.

use std::collections::HashMap;

use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use super::{slug_from_transcript_dir, Surface, WorkEvent, WorkKind};

/// The subset of a Claude Code hook payload we read. Claude Code adds fields over
/// time, so everything is optional and unknown fields are ignored — a forward-
/// compatible read, never a strict decode.
#[derive(Debug, Clone, Deserialize)]
pub struct HookPayload {
    /// `"UserPromptSubmit"`, `"Stop"`, `"Notification"`, … — the discriminator.
    pub hook_event_name: String,
    /// The session's working directory when the event fired.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Absolute path to the session transcript — its parent dir name matches the
    /// alias-map key (`claude_code.rs` keys aliases by transcript dir name).
    #[serde(default)]
    pub transcript_path: Option<String>,
    /// For `Notification` events: `"idle_prompt"`, `"permission_prompt"`, … Only
    /// the input-awaiting kinds become `AwaitingInput`.
    #[serde(default)]
    pub notification_type: Option<String>,
}

/// Map a hook payload to a `WorkEvent`, or `None` for events we don't act on.
///
/// `now_rfc3339` is the event timestamp, passed in so this stays pure (no clock
/// read) and unit-testable. `aliases` is the same transcript-dir-name → slug map
/// the transcript watcher uses, so hook-derived attribution matches it.
pub fn hook_to_event(
    p: &HookPayload,
    aliases: &HashMap<String, String>,
    now_rfc3339: &str,
) -> Option<WorkEvent> {
    let kind = match p.hook_event_name.as_str() {
        "UserPromptSubmit" => WorkKind::Prompt,
        "Stop" => WorkKind::AwaitingInput,
        // Only the "Claude wants you" notifications count as awaiting-input. Other
        // notification types (auth_success, …) aren't a status signal.
        "Notification"
            if matches!(
                p.notification_type.as_deref(),
                Some("idle_prompt" | "permission_prompt" | "elicitation_dialog")
            ) =>
        {
            WorkKind::AwaitingInput
        }
        "SessionStart" => WorkKind::SessionStart,
        "SessionEnd" => WorkKind::SessionEnd,
        // Tool events and everything else: ignored. Transcript-watch already covers
        // active-ness; folding tool hooks in here would just add noise.
        _ => return None,
    };

    // Attribution only matters for focus evidence; status-only events (Stop,
    // Notification, SessionEnd) don't move a block, so leave their project None.
    let project = if kind.is_focus_evidence() {
        resolve_slug(p, aliases)
    } else {
        None
    };

    Some(WorkEvent {
        ts: now_rfc3339.to_string(),
        surface: Surface::ClaudeCode,
        project,
        kind,
        confidence: 1.0, // cwd-derived attribution, same as the transcript adapter
        detail: None,
    })
}

/// Resolve a project slug for a hook event, mirroring the transcript adapter's
/// priority but from the hook payload's fields:
///   1. **Alias override** keyed by the transcript dir name (parent of
///      `transcript_path`) — the user's explicit last word.
///   2. **`cwd` basename** — the lossless launch-dir signal the hook hands us.
///   3. **Dir-name heuristic** — the lossy `slug_from_transcript_dir` fallback.
fn resolve_slug(p: &HookPayload, aliases: &HashMap<String, String>) -> Option<String> {
    let dir_name = p.transcript_path.as_deref().and_then(transcript_dir_name);
    if let Some(dir) = dir_name {
        if let Some(slug) = aliases.get(dir) {
            return Some(slug.clone());
        }
    }
    if let Some(slug) = p.cwd.as_deref().and_then(basename) {
        return Some(slug);
    }
    dir_name.and_then(slug_from_transcript_dir)
}

/// The transcript's parent directory name (the alias-map key), e.g.
/// `/home/u/.claude/projects/-root-Projects-waid/s.jsonl` → `-root-Projects-waid`.
fn transcript_dir_name(transcript_path: &str) -> Option<&str> {
    std::path::Path::new(transcript_path)
        .parent()?
        .file_name()?
        .to_str()
}

/// Trailing path segment of a directory, split on both separators so a Windows-
/// style cwd basenames correctly (matches `claude_code::slug_from_cwd`).
fn basename(cwd: &str) -> Option<String> {
    let base = cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next()?;
    (!base.is_empty()).then(|| base.to_string())
}

/// Bind the loopback hook endpoint and serve forever on a dedicated thread,
/// emitting `WorkEvent`s on `tx`. Returns once the socket is bound (an error means
/// the bind failed — e.g. the port is taken); the serving loop then runs for the
/// process lifetime. The spawned thread owns the `Server`, so the listener lives
/// as long as the process without a handle to hold.
///
/// Every request gets a `200` with an empty body — Claude Code ignores the
/// response, and the receiver's only contract is "accept fast, never block".
/// Malformed or unrecognized payloads are accepted and dropped, not rejected.
pub fn serve(
    tx: UnboundedSender<WorkEvent>,
    aliases: HashMap<String, String>,
    addr: &str,
) -> Result<(), String> {
    let server = tiny_http::Server::http(addr)
        .map_err(|e| format!("could not bind hook receiver on {addr}: {e}"))?;

    std::thread::Builder::new()
        .name("whence-hooks".into())
        .spawn(move || {
            for mut req in server.incoming_requests() {
                let mut body = String::new();
                // Read the POST body; on a read error just respond and move on.
                if req.as_reader().read_to_string(&mut body).is_ok() {
                    if let Ok(payload) = serde_json::from_str::<HookPayload>(&body) {
                        let now = chrono::Utc::now().to_rfc3339();
                        if let Some(ev) = hook_to_event(&payload, &aliases, &now) {
                            // Unbounded send never blocks; an error only means the
                            // core task is gone (shutdown), so dropping is correct.
                            let _ = tx.send(ev);
                        }
                    }
                }
                let _ = req.respond(tiny_http::Response::empty(200));
            }
        })
        .map_err(|e| format!("could not spawn hook receiver thread: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-06-26T12:00:00Z";

    fn payload(event: &str) -> HookPayload {
        HookPayload {
            hook_event_name: event.to_string(),
            cwd: Some("/root/Projects/whence".into()),
            transcript_path: Some(
                "/home/u/.claude/projects/-root-Projects-whence/s.jsonl".into(),
            ),
            notification_type: None,
        }
    }

    fn no_aliases() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn stop_is_awaiting_input_status_only() {
        let ev = hook_to_event(&payload("Stop"), &no_aliases(), NOW).unwrap();
        assert_eq!(ev.kind, WorkKind::AwaitingInput);
        assert_eq!(ev.surface, Surface::ClaudeCode);
        // Status-only: never attributed, never moves a block.
        assert_eq!(ev.project, None);
        assert!(!ev.kind.is_focus_evidence());
        assert_eq!(ev.ts, NOW);
    }

    #[test]
    fn prompt_is_focus_evidence_with_cwd_slug() {
        let ev = hook_to_event(&payload("UserPromptSubmit"), &no_aliases(), NOW).unwrap();
        assert_eq!(ev.kind, WorkKind::Prompt);
        assert!(ev.kind.is_focus_evidence());
        // cwd basename beats the lossy dir-name heuristic.
        assert_eq!(ev.project.as_deref(), Some("whence"));
    }

    #[test]
    fn prompt_alias_overrides_cwd() {
        let mut aliases = HashMap::new();
        aliases.insert("-root-Projects-whence".to_string(), "whence-canonical".to_string());
        let ev = hook_to_event(&payload("UserPromptSubmit"), &aliases, NOW).unwrap();
        assert_eq!(ev.project.as_deref(), Some("whence-canonical"));
    }

    #[test]
    fn prompt_falls_back_to_dir_name_without_cwd() {
        // Hyphenated dir name resolves lossily, but it's all we have without cwd.
        let mut p = payload("UserPromptSubmit");
        p.cwd = None;
        let ev = hook_to_event(&p, &no_aliases(), NOW).unwrap();
        assert_eq!(ev.project.as_deref(), Some("whence"));
    }

    #[test]
    fn notification_awaiting_only_for_input_kinds() {
        let mut idle = payload("Notification");
        idle.notification_type = Some("idle_prompt".into());
        assert_eq!(
            hook_to_event(&idle, &no_aliases(), NOW).unwrap().kind,
            WorkKind::AwaitingInput
        );

        let mut perm = payload("Notification");
        perm.notification_type = Some("permission_prompt".into());
        assert_eq!(
            hook_to_event(&perm, &no_aliases(), NOW).unwrap().kind,
            WorkKind::AwaitingInput
        );

        // A non-input notification (e.g. auth_success) is not a status signal.
        let mut other = payload("Notification");
        other.notification_type = Some("auth_success".into());
        assert!(hook_to_event(&other, &no_aliases(), NOW).is_none());
    }

    #[test]
    fn session_lifecycle_maps_through() {
        assert_eq!(
            hook_to_event(&payload("SessionStart"), &no_aliases(), NOW).unwrap().kind,
            WorkKind::SessionStart
        );
        assert_eq!(
            hook_to_event(&payload("SessionEnd"), &no_aliases(), NOW).unwrap().kind,
            WorkKind::SessionEnd
        );
    }

    #[test]
    fn tool_and_unknown_events_are_ignored() {
        assert!(hook_to_event(&payload("PreToolUse"), &no_aliases(), NOW).is_none());
        assert!(hook_to_event(&payload("PostToolUse"), &no_aliases(), NOW).is_none());
        assert!(hook_to_event(&payload("SomethingNew"), &no_aliases(), NOW).is_none());
    }

    #[test]
    fn payload_tolerates_unknown_and_missing_fields() {
        // Only hook_event_name is required; extra fields are ignored.
        let json = r#"{"hook_event_name":"Stop","session_id":"x","future_field":42}"#;
        let p: HookPayload = serde_json::from_str(json).unwrap();
        let ev = hook_to_event(&p, &no_aliases(), NOW).unwrap();
        assert_eq!(ev.kind, WorkKind::AwaitingInput);
    }
}
