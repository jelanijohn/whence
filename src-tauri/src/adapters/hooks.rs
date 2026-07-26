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
//!   * [`serve`] — the thin impure shell: bind `tiny_http`, check the bearer token
//!     (`crate::auth` — embedded in the installed hook URL), read bodies, map, send.
//!
//! Every event resolves a project slug — status-only ones (`Stop`/`Notification`)
//! included. The widget shows one row per project, so a turn-finished `Stop` must
//! mark *that* session "awaiting you"; focus-evidence events (`UserPromptSubmit` →
//! `Prompt`) likewise land the *active* switch on the right project without waiting
//! for the transcript debounce.

use std::collections::HashMap;

use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use super::{claude_code, slug_from_transcript_dir, Surface, WorkEvent, WorkKind};

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
    /// For `UserPromptSubmit`: the prompt text you typed. Only *retained* (as a
    /// sanitized snippet on `WorkEvent.detail`) when the opt-in hook-prompt
    /// context source is on — otherwise dropped at this boundary, never carried.
    #[serde(default)]
    pub prompt: Option<String>,
}

/// Map a hook payload to a `WorkEvent`, or `None` for events we don't act on.
///
/// `now_rfc3339` is the event timestamp, passed in so this stays pure (no clock
/// read) and unit-testable. `aliases` is the same transcript-dir-name → slug map
/// the transcript watcher uses, so hook-derived attribution matches it.
/// `transcripts_root` is *our* view of the transcript tree, for rebasing a
/// payload path minted on a different filesystem (see [`local_transcript_path`]).
///
/// `capture_context` is the live hook-prompt context gate (docs/context-strings.md
/// §10): when on, a `UserPromptSubmit` carries a sanitized snippet of your prompt
/// and a `SessionStart` the transcript's session summary, both on `detail` — the
/// orchestrator turns those into the displayed context string. When off, the
/// prompt text is dropped *here*, at the capture boundary, not downstream.
pub fn hook_to_event(
    p: &HookPayload,
    aliases: &HashMap<String, String>,
    transcripts_root: Option<&std::path::Path>,
    now_rfc3339: &str,
    capture_context: bool,
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

    // Resolve the project for *every* event, status-only ones included. The widget
    // now shows one row per project (§ multi-session), so a `Stop`/`Notification`
    // must say *which* session is awaiting you — its status lands on that project's
    // row. The payload carries `transcript_path` on these events, so resolution works
    // the same as for focus evidence.
    let project = resolve_slug(p, aliases, transcripts_root);

    // Context-moment capture (opt-in): the prompt snippet on a Prompt, the session
    // summary on a SessionStart (a resumed/compacted transcript names the thread
    // being continued — useful before you've typed anything). Sanitized and capped
    // at the same bound as every context string. `detail` rides through the engine
    // untouched (segment.rs never reads it) and the orchestrator harvests it.
    let detail = if capture_context {
        match kind {
            WorkKind::Prompt => p
                .prompt
                .as_deref()
                .filter(|s| !is_synthetic_prompt(s))
                .map(crate::context::sanitize)
                .filter(|s| !s.is_empty()),
            WorkKind::SessionStart => p
                .transcript_path
                .as_deref()
                .and_then(|t| {
                    claude_code::first_summary(&local_transcript_path(t, transcripts_root))
                })
                .map(|s| crate::context::sanitize(&s))
                .filter(|s| !s.is_empty()),
            _ => None,
        }
    } else {
        None
    };

    Some(WorkEvent {
        ts: now_rfc3339.to_string(),
        surface: Surface::ClaudeCode,
        project,
        // The transcript file stem is the session UUID — the same per-session source key
        // the transcript watcher emits, so a hook's status lands on the right source row.
        source: p.transcript_path.as_deref().and_then(transcript_stem),
        source_label: None, // engine labels it "claude code"
        kind,
        confidence: 1.0, // cwd-derived attribution, same as the transcript adapter
        detail,
    })
}

/// True when a `UserPromptSubmit` payload is harness-injected rather than typed.
/// Claude Code delivers synthetic turns — background-task notifications
/// (`<task-notification><task-id>…`), slash-command expansions
/// (`<command-name>…`), `!`-command output (`<local-command-stdout>…`) — through
/// the same hook as real prompts. Those describe the harness's plumbing, not what
/// you're working on, and because moments are last-writer-wins one would
/// supersede your actual prompt on the widget. Heuristic: the prompt *starts*
/// with an angle-bracket tag. The user-authored posture (docs/context-strings.md
/// §10) prefers a false drop (a pasted `<div>` snippet degrades to the git
/// fallback) over displaying machine text.
fn is_synthetic_prompt(prompt: &str) -> bool {
    prompt
        .trim_start()
        .strip_prefix('<')
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_alphabetic())
}

/// Resolve a project slug for a hook event, matching the transcript adapter's
/// resolution so hook-derived focus events attribute to the same project the
/// watcher does:
///   1. **Alias override** keyed by the transcript dir name (parent of
///      `transcript_path`) — the user's explicit last word.
///   2. **Transcript launch dir** — the basename of the *first* `cwd` line in the
///      transcript (`claude_code::slug_from_cwd`). This is the stable, lossless
///      source. The payload's own `cwd` field is the session's *live* cwd, which
///      drifts when the session `cd`s into a subdir (a `cargo` build under
///      `whence/` would mis-attribute the prompt to `src-tauri`); the launch dir
///      doesn't move.
///   3. **Dir-name heuristic** — the lossy `slug_from_transcript_dir` fallback.
///   4. **Live `cwd` basename** — last resort, only when there's no transcript to
///      read (so a drifted-but-real cwd still beats nothing).
fn resolve_slug(
    p: &HookPayload,
    aliases: &HashMap<String, String>,
    transcripts_root: Option<&std::path::Path>,
) -> Option<String> {
    if let Some(transcript_path) = p.transcript_path.as_deref() {
        let dir_name = transcript_dir_name(transcript_path);
        if let Some(dir) = dir_name {
            if let Some(slug) = aliases.get(dir) {
                return Some(slug.clone());
            }
        }
        if let Some(slug) =
            claude_code::slug_from_cwd(&local_transcript_path(transcript_path, transcripts_root))
        {
            return Some(slug);
        }
        if let Some(slug) = dir_name.and_then(slug_from_transcript_dir) {
            return Some(slug);
        }
    }
    p.cwd.as_deref().and_then(basename)
}

/// The transcript path *we* can read. The payload's `transcript_path` is the path
/// as Claude Code sees it — when Claude Code runs inside WSL and Whence natively
/// on the Windows host, that's a Linux path that doesn't exist here. If it isn't
/// readable directly, rebase its `<dir>/<file>` tail onto our own transcripts
/// root (the same `\\wsl$\...` tree the watcher reads), so the cwd-based slug
/// resolution keeps working across the filesystem boundary.
fn local_transcript_path(
    transcript_path: &str,
    transcripts_root: Option<&std::path::Path>,
) -> std::path::PathBuf {
    let p = std::path::Path::new(transcript_path);
    if p.is_file() {
        return p.to_path_buf();
    }
    if let (Some(root), Some(dir), Some(file)) = (
        transcripts_root,
        transcript_dir_name(transcript_path),
        std::path::Path::new(transcript_path).file_name(),
    ) {
        return root.join(dir).join(file);
    }
    p.to_path_buf()
}

/// The transcript file stem — the Claude Code session UUID, the per-session source
/// key, e.g. `…/-root-Projects-waid/abc123.jsonl` → `abc123`. Matches what the
/// transcript watcher emits as `source`.
fn transcript_stem(transcript_path: &str) -> Option<String> {
    std::path::Path::new(transcript_path)
        .file_stem()?
        .to_str()
        .map(str::to_string)
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
/// emitting `WorkEvent`s on `tx`. Returns the **bound address** once the socket is
/// up (so a `:0` bind reports its ephemeral port — how the e2e test avoids a fixed
/// port); an error means the bind failed — e.g. the port is taken. The serving
/// loop then runs for the process lifetime. The spawned thread owns the `Server`,
/// so the listener lives as long as the process without a handle to hold.
///
/// **Bearer-gated** (`crate::auth`): Claude Code `http` hooks can't set headers,
/// so `install_claude_hooks` embeds the token in the URL (`/hook/<token>`) and the
/// check accepts either carrier. Unauthenticated requests get a 401 and count a
/// denial — a forged `UserPromptSubmit` would otherwise be a confidence-1.0
/// you-acted `Prompt`, the strongest signal in the trust model. The token is read
/// per request from the shared handle so a Settings rotation applies live.
///
/// `settings` is the live shared handle, read per request for the hook-prompt
/// context gate (`context_strings && context_hook_prompts`) — a Settings toggle
/// starts/stops prompt-snippet retention without a restart. `moments` is where a
/// captured snippet/summary is recorded as its project's live context moment
/// (receivers record; the orchestrator arbitrates and clears — context.rs).
///
/// Every authorized request gets a `200` with an empty body — Claude Code ignores
/// the response, and the receiver's only contract is "accept fast, never block".
/// Malformed or unrecognized payloads are accepted and dropped, not rejected.
pub fn serve(
    tx: UnboundedSender<WorkEvent>,
    aliases: HashMap<String, String>,
    addr: &str,
    transcripts_root: Option<std::path::PathBuf>,
    token: crate::auth::SharedToken,
    denials: crate::auth::Denials,
    settings: std::sync::Arc<std::sync::Mutex<crate::settings::Settings>>,
    moments: crate::context::SharedMoments,
) -> Result<std::net::SocketAddr, String> {
    let server = tiny_http::Server::http(addr)
        .map_err(|e| format!("could not bind hook receiver on {addr}: {e}"))?;
    let bound = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| format!("hook receiver on {addr} bound to a non-IP address"))?;

    std::thread::Builder::new()
        .name("whence-hooks".into())
        .spawn(move || {
            for mut req in server.incoming_requests() {
                // Auth inside a scope: the read guard (poison-recovering, no clone)
                // drops before the body read, so a rotation never waits on a slow
                // caller.
                {
                    let tok = crate::auth::read_token(&token);
                    if !crate::auth::tiny_http_authorized(&req, &tok) {
                        crate::auth::record_denial(&denials, "hooks");
                        let _ = req.respond(tiny_http::Response::empty(401));
                        continue;
                    }
                }
                let mut body = String::new();
                // Read the POST body; on a read error just respond and move on.
                if req.as_reader().read_to_string(&mut body).is_ok() {
                    if let Ok(payload) = serde_json::from_str::<HookPayload>(&body) {
                        let now = chrono::Utc::now().to_rfc3339();
                        let capture = settings
                            .lock()
                            .map(|s| s.context_strings && s.context_hook_prompts)
                            .unwrap_or(false);
                        if let Some(ev) = hook_to_event(
                            &payload,
                            &aliases,
                            transcripts_root.as_deref(),
                            &now,
                            capture,
                        ) {
                            // A captured snippet/summary (detail is only set when the
                            // gate was on) becomes the project's live context moment.
                            if let (Some(project), Some(text), Some(ts)) =
                                (&ev.project, &ev.detail, ev.ts_secs())
                            {
                                crate::context::record_moment(
                                    &moments,
                                    project,
                                    text,
                                    crate::context::ContextSource::HookPrompt,
                                    ts,
                                );
                            }
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

    Ok(bound)
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
            prompt: None,
        }
    }

    fn no_aliases() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn stop_is_awaiting_input_and_carries_its_project() {
        let ev = hook_to_event(&payload("Stop"), &no_aliases(), None, NOW, false).unwrap();
        assert_eq!(ev.kind, WorkKind::AwaitingInput);
        assert_eq!(ev.surface, Surface::ClaudeCode);
        // Status-only, but still attributed so its row knows which session is awaiting.
        assert_eq!(ev.project.as_deref(), Some("whence"));
        // The transcript stem is the source key — the same one the watcher emits, so the
        // hook's status lands on the right session's source row.
        assert_eq!(ev.source.as_deref(), Some("s"));
        assert!(!ev.kind.is_focus_evidence());
        assert_eq!(ev.ts, NOW);
    }

    #[test]
    fn prompt_is_focus_evidence_resolves_project() {
        // payload()'s transcript_path doesn't exist, so resolution falls through to
        // the dir-name heuristic — still "whence".
        let ev = hook_to_event(&payload("UserPromptSubmit"), &no_aliases(), None, NOW, false).unwrap();
        assert_eq!(ev.kind, WorkKind::Prompt);
        assert!(ev.kind.is_focus_evidence());
        assert_eq!(ev.project.as_deref(), Some("whence"));
    }

    #[test]
    fn prompt_uses_transcript_launch_dir_not_drifted_cwd() {
        use std::io::Write;
        // A real transcript whose launch cwd is the project root, paired with a
        // payload cwd that has drifted into a subdir (the live-observed bug: a
        // `cd src-tauri` made the hook attribute the prompt to "src-tauri").
        // Resolution must follow the stable launch dir, like the transcript watcher.
        // The temp dir root is unique to this test — claude_code's tests also stage
        // a `-root-Projects-blapp-web`, and sharing one path made the suite flaky
        // (each test deletes the dir under the other mid-run).
        let root = std::env::temp_dir().join("whence-hooks-drift-root");
        let dir = root.join("-root-Projects-blapp-web");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks-session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{{\"type\":\"meta\"}}").unwrap();
        writeln!(f, "{{\"type\":\"user\",\"cwd\":\"/root/Projects/blapp-web\"}}").unwrap();

        let p = HookPayload {
            hook_event_name: "UserPromptSubmit".into(),
            cwd: Some("/root/Projects/blapp-web/packages/api".into()), // drifted
            transcript_path: Some(path.to_string_lossy().into_owned()),
            notification_type: None,
            prompt: None,
        };
        let ev = hook_to_event(&p, &no_aliases(), None, NOW, false).unwrap();
        // Launch dir "blapp-web" beats the drifted cwd ("api") and lossy dir-name ("web").
        assert_eq!(ev.project.as_deref(), Some("blapp-web"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn foreign_transcript_path_rebases_onto_local_root() {
        use std::io::Write;
        // Cross-filesystem case: the payload carries the path as Claude Code (in
        // WSL) sees it, but the transcript is actually readable under *our* root
        // (the `\\wsl$\...` tree on a Windows host). The cwd read must follow the
        // rebased path, not give up and fall back to the lossy dir name.
        let root = std::env::temp_dir().join("whence-hooks-rebase-root");
        let dir = root.join("-root-Projects-blapp-web");
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join("rebase-session.jsonl")).unwrap();
        writeln!(f, "{{\"type\":\"user\",\"cwd\":\"/root/Projects/blapp-web\"}}").unwrap();

        let p = HookPayload {
            hook_event_name: "UserPromptSubmit".into(),
            cwd: None,
            // A path from the other filesystem — does not exist here.
            transcript_path: Some(
                "/root/.claude/projects/-root-Projects-blapp-web/rebase-session.jsonl".into(),
            ),
            notification_type: None,
            prompt: None,
        };
        let ev = hook_to_event(&p, &no_aliases(), Some(&root), NOW, false).unwrap();
        // Rebased cwd read gives "blapp-web"; the dir-name fallback would say "web".
        assert_eq!(ev.project.as_deref(), Some("blapp-web"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn prompt_without_transcript_falls_back_to_live_cwd() {
        // No transcript to read → the (possibly drifted) live cwd still beats nothing.
        let mut p = payload("UserPromptSubmit");
        p.transcript_path = None;
        p.cwd = Some("/root/Projects/glue".into());
        let ev = hook_to_event(&p, &no_aliases(), None, NOW, false).unwrap();
        assert_eq!(ev.project.as_deref(), Some("glue"));
    }

    #[test]
    fn prompt_alias_overrides_cwd() {
        let mut aliases = HashMap::new();
        aliases.insert("-root-Projects-whence".to_string(), "whence-canonical".to_string());
        let ev = hook_to_event(&payload("UserPromptSubmit"), &aliases, None, NOW, false).unwrap();
        assert_eq!(ev.project.as_deref(), Some("whence-canonical"));
    }

    #[test]
    fn prompt_falls_back_to_dir_name_without_cwd() {
        // Hyphenated dir name resolves lossily, but it's all we have without cwd.
        let mut p = payload("UserPromptSubmit");
        p.cwd = None;
        let ev = hook_to_event(&p, &no_aliases(), None, NOW, false).unwrap();
        assert_eq!(ev.project.as_deref(), Some("whence"));
    }

    #[test]
    fn notification_awaiting_only_for_input_kinds() {
        let mut idle = payload("Notification");
        idle.notification_type = Some("idle_prompt".into());
        assert_eq!(
            hook_to_event(&idle, &no_aliases(), None, NOW, false).unwrap().kind,
            WorkKind::AwaitingInput
        );

        let mut perm = payload("Notification");
        perm.notification_type = Some("permission_prompt".into());
        assert_eq!(
            hook_to_event(&perm, &no_aliases(), None, NOW, false).unwrap().kind,
            WorkKind::AwaitingInput
        );

        // A non-input notification (e.g. auth_success) is not a status signal.
        let mut other = payload("Notification");
        other.notification_type = Some("auth_success".into());
        assert!(hook_to_event(&other, &no_aliases(), None, NOW, false).is_none());
    }

    #[test]
    fn session_lifecycle_maps_through() {
        assert_eq!(
            hook_to_event(&payload("SessionStart"), &no_aliases(), None, NOW, false).unwrap().kind,
            WorkKind::SessionStart
        );
        assert_eq!(
            hook_to_event(&payload("SessionEnd"), &no_aliases(), None, NOW, false).unwrap().kind,
            WorkKind::SessionEnd
        );
    }

    #[test]
    fn tool_and_unknown_events_are_ignored() {
        assert!(hook_to_event(&payload("PreToolUse"), &no_aliases(), None, NOW, false).is_none());
        assert!(hook_to_event(&payload("PostToolUse"), &no_aliases(), None, NOW, false).is_none());
        assert!(hook_to_event(&payload("SomethingNew"), &no_aliases(), None, NOW, false).is_none());
    }

    #[test]
    fn prompt_snippet_captured_only_when_gated_on() {
        let mut p = payload("UserPromptSubmit");
        p.prompt = Some("  fix the\nHEAD parser\t\u{7} fixtures  ".into());

        // Gate off (the default): the prompt text is dropped at this boundary.
        let ev = hook_to_event(&p, &no_aliases(), None, NOW, false).unwrap();
        assert_eq!(ev.detail, None);

        // Gate on: a sanitized single-line snippet rides on detail.
        let ev = hook_to_event(&p, &no_aliases(), None, NOW, true).unwrap();
        assert_eq!(ev.detail.as_deref(), Some("fix the HEAD parser fixtures"));

        // An empty/whitespace prompt yields no snippet, not an empty string.
        p.prompt = Some("   \n ".into());
        let ev = hook_to_event(&p, &no_aliases(), None, NOW, true).unwrap();
        assert_eq!(ev.detail, None);

        // Status events never carry a snippet even when gated on.
        let mut stop = payload("Stop");
        stop.prompt = Some("should never appear".into());
        let ev = hook_to_event(&stop, &no_aliases(), None, NOW, true).unwrap();
        assert_eq!(ev.detail, None);
    }

    #[test]
    fn synthetic_harness_prompts_never_become_moments() {
        let mut p = payload("UserPromptSubmit");
        // Harness-injected turns fire UserPromptSubmit too; each would otherwise
        // supersede the real prompt moment (last-writer-wins).
        for synthetic in [
            "<task-notification> <task-id>a40bcbb3</task-id> done",
            "  <command-name>/commit</command-name>",
            "<local-command-stdout>ok</local-command-stdout>",
            "<system-reminder>background job finished</system-reminder>",
        ] {
            p.prompt = Some(synthetic.into());
            let ev = hook_to_event(&p, &no_aliases(), None, NOW, true).unwrap();
            assert_eq!(ev.detail, None, "captured synthetic prompt: {synthetic}");
            // The focus evidence itself still flows — only the snippet is dropped.
            assert_eq!(ev.kind, WorkKind::Prompt);
        }

        // Non-tag uses of `<` are still user text and keep their snippet.
        p.prompt = Some("< 5 retries then bail".into());
        let ev = hook_to_event(&p, &no_aliases(), None, NOW, true).unwrap();
        assert_eq!(ev.detail.as_deref(), Some("< 5 retries then bail"));
    }

    #[test]
    fn session_start_captures_transcript_summary_when_gated_on() {
        use std::io::Write;
        // A resumed transcript with a head summary line — the SessionStart moment.
        let root = std::env::temp_dir().join("whence-hooks-summary-root");
        let dir = root.join("-root-Projects-whence");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("resumed.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{{\"type\":\"summary\",\"summary\":\"Context strings v1\"}}").unwrap();
        writeln!(f, "{{\"type\":\"user\",\"cwd\":\"/root/Projects/whence\"}}").unwrap();

        let mut p = payload("SessionStart");
        p.transcript_path = Some(path.to_string_lossy().into_owned());

        let ev = hook_to_event(&p, &no_aliases(), None, NOW, true).unwrap();
        assert_eq!(ev.detail.as_deref(), Some("Context strings v1"));
        // Gate off → no summary retained.
        let ev = hook_to_event(&p, &no_aliases(), None, NOW, false).unwrap();
        assert_eq!(ev.detail, None);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn payload_tolerates_unknown_and_missing_fields() {
        // Only hook_event_name is required; extra fields are ignored.
        let json = r#"{"hook_event_name":"Stop","session_id":"x","future_field":42}"#;
        let p: HookPayload = serde_json::from_str(json).unwrap();
        let ev = hook_to_event(&p, &no_aliases(), None, NOW, false).unwrap();
        assert_eq!(ev.kind, WorkKind::AwaitingInput);
    }

    /// One raw HTTP round-trip (Connection: close, so the read terminates).
    fn raw_http(addr: &str, request: &str) -> String {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).unwrap();
        resp
    }

    #[test]
    fn serve_gates_requests_end_to_end() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::{Arc, RwLock};

        // A real listener on an ephemeral port (`:0` — the OS picks a free one,
        // reported back by `serve`), gated by a known token.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let token: crate::auth::SharedToken = Arc::new(RwLock::new("testtok".to_string()));
        let denials: crate::auth::Denials = Arc::new(AtomicU64::new(0));
        let settings = Arc::new(std::sync::Mutex::new(crate::settings::Settings::default()));
        let moments = crate::context::new_moments();
        let bound = serve(
            tx,
            no_aliases(),
            "127.0.0.1:0",
            None,
            token.clone(),
            denials.clone(),
            settings.clone(),
            moments.clone(),
        )
        .unwrap();
        let addr = &bound.to_string();

        let body = r#"{"hook_event_name":"Stop","cwd":"/root/Projects/whence"}"#;
        let post = |path: &str, extra: &str| {
            format!(
                "POST {path} HTTP/1.1\r\nHost: whence\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        };

        // No credential → 401, a counted denial, and no event reaches the engine.
        let resp = raw_http(addr, &post("/hook", ""));
        assert!(resp.starts_with("HTTP/1.1 401"), "expected 401, got: {resp}");
        assert_eq!(denials.load(Ordering::Relaxed), 1);
        assert!(rx.try_recv().is_err(), "unauthenticated event must be dropped");

        // Wrong path token → still 401.
        let resp = raw_http(addr, &post("/hook/wrong", ""));
        assert!(resp.starts_with("HTTP/1.1 401"));
        assert_eq!(denials.load(Ordering::Relaxed), 2);

        // The installed-URL carrier (path token) → 200 and the event flows.
        // The serve loop sends before responding, so the event is observable here.
        let resp = raw_http(addr, &post("/hook/testtok", ""));
        assert!(resp.starts_with("HTTP/1.1 200"), "expected 200, got: {resp}");
        let ev = rx.try_recv().expect("authorized event must flow");
        assert_eq!(ev.kind, WorkKind::AwaitingInput);

        // The header carrier → 200 as well.
        let resp = raw_http(addr, &post("/hook", "Authorization: Bearer testtok\r\n"));
        assert!(resp.starts_with("HTTP/1.1 200"));
        assert!(rx.try_recv().is_ok());

        // Rotation applies live: swap the shared token, old one now denied.
        *token.write().unwrap() = "rotated".to_string();
        let resp = raw_http(addr, &post("/hook/testtok", ""));
        assert!(resp.starts_with("HTTP/1.1 401"));
        let resp = raw_http(addr, &post("/hook/rotated", ""));
        assert!(resp.starts_with("HTTP/1.1 200"));

        // The moment gate applies live too: with the gate off (defaults) a prompt
        // recorded nothing; flip it on and the snippet lands in the moments map.
        let prompt_body = r#"{"hook_event_name":"UserPromptSubmit","cwd":"/root/Projects/whence","prompt":"fix the parser"}"#;
        let post_prompt = format!(
            "POST /hook/rotated HTTP/1.1\r\nHost: whence\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prompt_body}",
            prompt_body.len()
        );
        let resp = raw_http(addr, &post_prompt);
        assert!(resp.starts_with("HTTP/1.1 200"));
        assert!(crate::context::moment_for(&moments, "whence").is_none(), "gate off → no moment");
        settings.lock().unwrap().context_hook_prompts = true;
        let resp = raw_http(addr, &post_prompt);
        assert!(resp.starts_with("HTTP/1.1 200"));
        let moment = crate::context::moment_for(&moments, "whence").expect("gate on → moment");
        assert_eq!(moment.text, "fix the parser");
        assert_eq!(moment.source, crate::context::ContextSource::HookPrompt);
    }
}
