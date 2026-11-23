//! **Codex / ChatGPT Work** hook adapter — local sessions only.
//!
//! OpenAI's Codex runtime (the CLI, and ChatGPT Desktop's *Work → Local* mode)
//! runs lifecycle hooks from `$CODEX_HOME/hooks.json`. Unlike Claude Code it has
//! no `http` hook type: a hook is a `command` that receives the event JSON on
//! stdin. So `commands::install_codex_hooks` writes a `curl` one-liner that pipes
//! that stdin to the *existing* hooks receiver (`hooks.rs`, `127.0.0.1:18450`)
//! under a `/codex` path, with the bearer token as a header. `hooks::serve` routes
//! `/codex` requests here — no second listener.
//!
//! Codex has no transcript watcher on our side (its rollout format is documented
//! as unstable), so hooks are the *only* signal: `PostToolUse` is mapped (as
//! `ToolUse`) where the Claude Code path ignores it.
//!
//! Cloud-orchestrated sessions and ordinary ChatGPT chats never run local hooks,
//! so they never reach this adapter. Nothing in the payload identifies the client
//! (desktop app vs CLI), so every session is just "codex".
//!
//! The installed hooks are `async` (backgrounded `curl`s), so two events from one
//! session can land out of order. [`TurnGuard`] drops the stale ones — keyed on
//! the payload's `turn_id` — before they reach the mapper.
//!
//! [`codex_hook_to_event`] and [`TurnGuard`] are **pure and fixture-tested**; the
//! socket lives in `hooks.rs`.

use std::collections::HashMap;

use serde::Deserialize;

use super::{collapse_worktree_cwd, slugify, Surface, WorkEvent, WorkKind};

/// The subset of a Codex hook payload we read. Lenient like `hooks::HookPayload`:
/// everything but the discriminator is optional and unknown fields are ignored.
///
/// Deliberately *absent*: `tool_input`, `tool_response`, `last_assistant_message`,
/// `transcript_path`. The hook pipes the whole payload over loopback, but anything
/// not named here is discarded by the deserializer and never held in memory as a
/// field — the read-scope boundary for this adapter.
#[derive(Debug, Clone, Deserialize)]
pub struct CodexHookPayload {
    /// `"UserPromptSubmit"`, `"PostToolUse"`, `"Stop"`, … — the discriminator.
    pub hook_event_name: String,
    /// The Codex thread id — the per-session source key (subagent hooks report the
    /// parent session's id, so they fold onto its row).
    #[serde(default)]
    pub session_id: Option<String>,
    /// The session's working directory — the only attribution input.
    #[serde(default)]
    pub cwd: Option<String>,
    /// The turn this event belongs to (turn-scoped events only). Read solely by
    /// [`TurnGuard`] to order events; never placed on a `WorkEvent`.
    #[serde(default)]
    pub turn_id: Option<String>,
    /// For `UserPromptSubmit`: the prompt text. Only *retained* (sanitized, on
    /// `WorkEvent.detail`) when the opt-in hook-prompt context source is on.
    #[serde(default)]
    pub prompt: Option<String>,
}

/// What [`TurnGuard`] knows about one turn of a session.
#[derive(Debug)]
struct Turn {
    id: String,
    /// Its `UserPromptSubmit` has arrived. `false` = we only know the turn from a
    /// tool event or `Stop` that beat the (backgrounded) prompt hook here.
    prompted: bool,
    /// Its `Stop` has arrived (admitted or not).
    stopped: bool,
}

/// Per-session ordering state for [`TurnGuard`].
#[derive(Debug, Default)]
struct SessionTurns {
    /// The newest turn we know of — the only one whose tool/stop events may
    /// touch the row. `None` until a turn id is seen, and again after a prompt
    /// that carried none (nothing to compare against → everything admitted).
    current: Option<String>,
    /// Recent turns, oldest first, bounded by [`TurnGuard::MAX_TURNS`].
    turns: std::collections::VecDeque<Turn>,
    /// `SessionEnd` seen; only a new prompt or `SessionStart` revives the session.
    ended: bool,
}

impl SessionTurns {
    fn turn_mut(&mut self, id: &str) -> Option<&mut Turn> {
        self.turns.iter_mut().find(|t| t.id == id)
    }

    /// Remember a newly-seen turn, evicting the oldest past the bound.
    fn remember(&mut self, id: &str, prompted: bool, stopped: bool) {
        if self.turns.len() >= TurnGuard::MAX_TURNS {
            self.turns.pop_front();
        }
        self.turns.push_back(Turn { id: id.to_string(), prompted, stopped });
    }

    /// Whether the current turn has already stopped (the row should read awaiting).
    fn current_stopped(&self) -> bool {
        self.current
            .as_deref()
            .and_then(|c| self.turns.iter().find(|t| t.id == c))
            .is_some_and(|t| t.stopped)
    }
}

/// [`TurnGuard::admit`]'s decision for one payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// In order — map it.
    Admit,
    /// A prompt that arrived *after* its turn (or a newer one) already stopped.
    /// Still mapped — it carries the you-acted attribution and the context
    /// snippet — but the receiver must follow it with an `AwaitingInput` so the
    /// finished row doesn't read "working" (see [`awaiting_followup`]).
    AdmitThenAwaiting,
    /// Out-of-order delivery — drop it.
    Drop,
}

/// Orders out-of-order Codex hook deliveries.
///
/// Why this exists: every installed hook is a backgrounded `curl`, so arrival
/// order is not event order — for *every* event, the prompt included. Unguarded,
/// these races corrupt a row's status:
///
/// * a late `PostToolUse` landing after its turn's `Stop` flips the row back to
///   *active* with nothing left to correct it (the turn is over, no further
///   event comes) — it reads "working" until the idle timeout;
/// * a late `UserPromptSubmit` landing after its own turn's `Stop` (a very
///   short turn) does the same;
/// * a late `Stop` landing after the *next* prompt marks a working row
///   *awaiting you*;
/// * anything landing after `SessionEnd` re-creates the row the end just dropped.
///
/// The engine can't arbitrate these — a blanket "tool use never overrides
/// awaiting" rule (the Claude Code trailing-write guard in `segment.rs`) would
/// also swallow the legitimate `PermissionRequest` → approve → `PostToolUse`
/// resume, which happens *within* one turn. Only `turn_id` tells the two apart,
/// and it is adapter-native, so the ordering lives here and the engine stays
/// untouched.
///
/// Model, per `session_id`: a bounded history of recent turns (prompted?
/// stopped?) and the *current* turn — the newest one known. A turn id never
/// seen before is by definition newer than everything known, whichever of its
/// events arrives first, so it becomes current. Rules:
///
/// * `UserPromptSubmit` / `SessionStart` are **never dropped** — they are the
///   you-acted signals.
///   * first sight of the turn → it becomes current;
///   * the turn is already known but its prompt wasn't (a tool event or `Stop`
///     beat it here) → a *late* prompt: mapped, and if the current turn has
///     already stopped, followed by an awaiting re-assert
///     ([`Verdict::AdmitThenAwaiting`]);
///   * the turn's prompt was already seen → a re-prompt on a reused `turn_id`:
///     the turn reopens and becomes current, so the guard self-heals should a
///     runtime ever reuse ids;
///   * no `turn_id` → current is cleared, so the events that follow are
///     admitted rather than compared against a stale turn.
/// * `PostToolUse` / `PermissionRequest` → dropped if their turn has stopped or
///   has been superseded by a newer current turn.
/// * `Stop` → always *recorded* on its turn; dropped if that turn is no longer
///   current (superseded), so its stragglers are still recognised later.
/// * After `SessionEnd`, everything but a prompt or `SessionStart` → dropped.
///
/// Every rule needs the ids it compares to be present; a payload without
/// `session_id`, or a turn-scoped event without `turn_id`, is admitted
/// unchanged — the guard degrades to the unguarded behaviour, never to dropping
/// live signal. A turn evicted from the bounded history is likewise treated as
/// unseen.
///
/// Pure: no I/O, no clock. Owned by the receiver thread (`hooks::serve`), which
/// handles requests one at a time, so it needs no lock.
#[derive(Debug, Default)]
pub struct TurnGuard {
    sessions: HashMap<String, SessionTurns>,
}

impl TurnGuard {
    /// Sessions remembered before pruning. Far above any real concurrent count;
    /// the bound only stops a long-running widget accumulating tombstones.
    const MAX_SESSIONS: usize = 256;
    /// Turns remembered per session. A straggler would have to trail its turn by
    /// more than this many newer turns to slip through — hooks time out in
    /// seconds, so that isn't a real delivery.
    const MAX_TURNS: usize = 32;

    /// Record `p` and decide whether it is mapped, mapped-then-corrected, or
    /// dropped as an out-of-order delivery.
    pub fn admit(&mut self, p: &CodexHookPayload) -> Verdict {
        let Some(session) = p.session_id.as_deref().filter(|s| !s.is_empty()) else {
            return Verdict::Admit;
        };
        let turn = p.turn_id.as_deref().filter(|t| !t.is_empty());
        let event = p.hook_event_name.as_str();
        if !matches!(
            event,
            "SessionStart"
                | "UserPromptSubmit"
                | "PostToolUse"
                | "PermissionRequest"
                | "Stop"
                | "SessionEnd"
        ) {
            return Verdict::Admit; // the mapper ignores it; nothing to order
        }

        if !self.sessions.contains_key(session) && self.sessions.len() >= Self::MAX_SESSIONS {
            // Ended sessions go first; if that isn't enough, forget everything —
            // the guard then simply starts over (unguarded until re-learned).
            self.sessions.retain(|_, s| !s.ended);
            if self.sessions.len() >= Self::MAX_SESSIONS {
                self.sessions.clear();
            }
        }
        let s = self.sessions.entry(session.to_string()).or_default();

        match event {
            "SessionStart" => {
                *s = SessionTurns::default();
                Verdict::Admit
            }
            "UserPromptSubmit" => {
                s.ended = false;
                let Some(t) = turn else {
                    s.current = None;
                    return Verdict::Admit;
                };
                match s.turn_mut(t) {
                    None => {
                        s.remember(t, true, false);
                        s.current = Some(t.to_string());
                        Verdict::Admit
                    }
                    // Re-prompt on a known, already-prompted turn: reopen it.
                    Some(known) if known.prompted => {
                        known.stopped = false;
                        s.current = Some(t.to_string());
                        Verdict::Admit
                    }
                    // Late prompt: the turn's tool event or Stop got here first.
                    Some(known) => {
                        known.prompted = true;
                        if s.current_stopped() {
                            Verdict::AdmitThenAwaiting
                        } else {
                            Verdict::Admit
                        }
                    }
                }
            }
            _ if s.ended => Verdict::Drop,
            "SessionEnd" => {
                s.ended = true;
                Verdict::Admit
            }
            _ => {
                // Stop | PostToolUse | PermissionRequest — turn-scoped.
                let Some(t) = turn else {
                    return Verdict::Admit;
                };
                let is_stop = event == "Stop";
                let Some(known) = s.turn_mut(t) else {
                    // Unseen turn → newer than all we know; it becomes current.
                    s.remember(t, false, is_stop);
                    s.current = Some(t.to_string());
                    return Verdict::Admit;
                };
                let already_stopped = known.stopped;
                if is_stop {
                    known.stopped = true; // recorded even when superseded
                }
                let superseded = s.current.as_deref().is_some_and(|c| c != t);
                if superseded || (already_stopped && !is_stop) {
                    Verdict::Drop
                } else {
                    Verdict::Admit
                }
            }
        }
    }
}

/// The correction that follows a [`Verdict::AdmitThenAwaiting`] prompt: the same
/// row, re-asserted as awaiting. Carries no `detail` — the prompt event already
/// delivered the context snippet, and status events never carry text.
pub fn awaiting_followup(prompt_event: &WorkEvent) -> WorkEvent {
    WorkEvent {
        kind: WorkKind::AwaitingInput,
        detail: None,
        ..prompt_event.clone()
    }
}

/// Map a Codex hook payload to a `WorkEvent`, or `None` for events we don't act
/// on (and for a payload with no resolvable project — Codex has no second
/// attribution source to fall back to, and an unattributed you-acted `Prompt`
/// would only paint a project-less row).
///
/// `now_rfc3339` comes in as a parameter so this stays pure. `aliases` is the
/// shared `project_aliases` map, consulted here by **cwd basename** (Codex has no
/// transcript-dir name to key on). `capture_context` is the same live gate the
/// Claude Code path uses (`context_strings && context_hook_prompts`).
pub fn codex_hook_to_event(
    p: &CodexHookPayload,
    aliases: &HashMap<String, String>,
    now_rfc3339: &str,
    capture_context: bool,
) -> Option<WorkEvent> {
    let kind = match p.hook_event_name.as_str() {
        "SessionStart" => WorkKind::SessionStart,
        "UserPromptSubmit" => WorkKind::Prompt,
        // The only activity signal between prompt and stop — autonomous evidence,
        // and what flips a row back to active after an approved permission.
        "PostToolUse" => WorkKind::ToolUse,
        // Codex has no `Notification` event; an approval request is its
        // "waiting on you" signal mid-turn.
        "PermissionRequest" => WorkKind::AwaitingInput,
        // The turn finished. Like Claude Code's `Stop`, the response may be a
        // question — this is status, not task completion.
        "Stop" => WorkKind::AwaitingInput,
        "SessionEnd" => WorkKind::SessionEnd,
        // PreToolUse, compaction, subagent and model-request events: ignored.
        _ => return None,
    };

    let project = resolve_slug(p.cwd.as_deref()?, aliases)?;

    let detail = if capture_context && kind == WorkKind::Prompt {
        p.prompt
            .as_deref()
            .filter(|s| !super::hooks::is_synthetic_prompt(s))
            .map(crate::context::sanitize)
            .filter(|s| !s.is_empty())
    } else {
        None
    };

    Some(WorkEvent {
        ts: now_rfc3339.to_string(),
        surface: Surface::Codex,
        project: Some(project),
        source: p.session_id.clone().filter(|s| !s.is_empty()),
        source_label: None, // engine labels it "codex"
        kind,
        confidence: 1.0, // cwd-derived attribution
        detail,
    })
}

/// cwd → slug: alias override by cwd basename, else the shared `slugify` of that
/// basename. A Claude-style managed-worktree path collapses to its repo first, so
/// a Codex session launched inside one attributes the same way Claude Code does.
fn resolve_slug(cwd: &str, aliases: &HashMap<String, String>) -> Option<String> {
    let base = collapse_worktree_cwd(cwd)
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()?;
    if let Some(slug) = aliases.get(base) {
        return Some(slug.clone());
    }
    slugify(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-11-22T12:00:00Z";

    fn payload(event: &str) -> CodexHookPayload {
        CodexHookPayload {
            hook_event_name: event.to_string(),
            session_id: Some("thr_123".into()),
            cwd: Some("/Users/u/Projects/whence".into()),
            turn_id: None,
            prompt: None,
        }
    }

    /// A payload for `event` on session `thr_123`, turn `turn`.
    fn turn(event: &str, turn: &str) -> CodexHookPayload {
        CodexHookPayload { turn_id: Some(turn.into()), ..payload(event) }
    }

    /// Whether the guard lets `p` through (admitted, with or without a follow-up).
    fn ok(g: &mut TurnGuard, p: &CodexHookPayload) -> bool {
        g.admit(p) != Verdict::Drop
    }

    #[test]
    fn guard_admits_an_in_order_turn() {
        let mut g = TurnGuard::default();
        assert!(ok(&mut g, &payload("SessionStart")));
        for ev in ["UserPromptSubmit", "PostToolUse", "PermissionRequest", "PostToolUse", "Stop"] {
            assert!(ok(&mut g, &turn(ev, "t1")), "{ev} in order");
        }
        // The next turn starts clean.
        for ev in ["UserPromptSubmit", "PostToolUse", "Stop"] {
            assert!(ok(&mut g, &turn(ev, "t2")), "{ev} in turn 2");
        }
        assert!(ok(&mut g, &payload("SessionEnd")));
    }

    #[test]
    fn guard_drops_tool_use_arriving_after_its_turns_stop() {
        // The race: Stop set awaiting_input; a late PostToolUse of the same turn
        // must not flip the row back to active.
        let mut g = TurnGuard::default();
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t1")));
        assert!(ok(&mut g, &turn("Stop", "t1")));
        assert!(!ok(&mut g, &turn("PostToolUse", "t1")));
        assert!(!ok(&mut g, &turn("PermissionRequest", "t1")));
        // …while a mid-turn approval resume (same turn, not stopped) still flows.
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t2")));
        assert!(ok(&mut g, &turn("PermissionRequest", "t2")));
        assert!(ok(&mut g, &turn("PostToolUse", "t2")));
    }

    #[test]
    fn guard_drops_a_stop_superseded_by_the_next_prompt() {
        let mut g = TurnGuard::default();
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t1")));
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t2"))); // t1's Stop still in flight
        assert!(!ok(&mut g, &turn("Stop", "t1")), "late Stop must not mark t2 awaiting");
        assert!(ok(&mut g, &turn("PostToolUse", "t2")));
        assert!(ok(&mut g, &turn("Stop", "t2")));
    }

    #[test]
    fn guard_drops_stragglers_after_session_end_until_revived() {
        let mut g = TurnGuard::default();
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t1")));
        assert!(ok(&mut g, &payload("SessionEnd")));
        assert!(!ok(&mut g, &turn("PostToolUse", "t1")));
        assert!(!ok(&mut g, &turn("Stop", "t1")));
        assert!(!ok(&mut g, &payload("SessionEnd")), "duplicate end");
        // A resumed session comes back on a prompt (or a SessionStart).
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t2")));
        assert!(ok(&mut g, &turn("PostToolUse", "t2")));
    }

    #[test]
    fn guard_never_drops_a_prompt_and_self_heals_on_a_reused_turn_id() {
        let mut g = TurnGuard::default();
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t1")));
        assert!(ok(&mut g, &turn("Stop", "t1")));
        // Same turn id again: the prompt is admitted and reopens the turn.
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t1")));
        assert!(ok(&mut g, &turn("PostToolUse", "t1")));
        assert!(ok(&mut g, &turn("Stop", "t1")));
    }

    #[test]
    fn guard_degrades_to_admit_without_ids_and_isolates_sessions() {
        let mut g = TurnGuard::default();
        // No turn ids at all → nothing to compare, everything flows.
        for ev in ["UserPromptSubmit", "Stop", "PostToolUse", "Stop"] {
            assert!(ok(&mut g, &payload(ev)), "{ev} without turn_id");
        }
        // No session id → never tracked.
        let mut anon = turn("PostToolUse", "t1");
        anon.session_id = None;
        assert!(ok(&mut g, &anon));
        // One session's stop doesn't gate another's same-named turn.
        assert!(ok(&mut g, &turn("Stop", "t9")));
        let mut other = turn("PostToolUse", "t9");
        other.session_id = Some("thr_other".into());
        assert!(ok(&mut g, &other));
        // Events the mapper ignores pass through untracked.
        assert!(ok(&mut g, &turn("PreToolUse", "t9")));
    }

    #[test]
    fn guard_keeps_a_finished_turn_awaiting_when_its_prompt_arrives_late() {
        // A very short turn: Stop(t1) beats the backgrounded UserPromptSubmit(t1).
        let mut g = TurnGuard::default();
        assert_eq!(g.admit(&turn("Stop", "t1")), Verdict::Admit);
        // The prompt still maps (attribution + context) but must be followed by
        // an awaiting re-assert, and the turn stays closed to stragglers.
        assert_eq!(g.admit(&turn("UserPromptSubmit", "t1")), Verdict::AdmitThenAwaiting);
        assert_eq!(g.admit(&turn("PostToolUse", "t1")), Verdict::Drop);
        // Same when a tool event was the first sign of the turn.
        assert_eq!(g.admit(&turn("PostToolUse", "t2")), Verdict::Admit);
        assert_eq!(g.admit(&turn("Stop", "t2")), Verdict::Admit);
        assert_eq!(g.admit(&turn("UserPromptSubmit", "t2")), Verdict::AdmitThenAwaiting);
        // A late prompt for a turn still running needs no correction.
        assert_eq!(g.admit(&turn("PostToolUse", "t3")), Verdict::Admit);
        assert_eq!(g.admit(&turn("UserPromptSubmit", "t3")), Verdict::Admit);
        assert_eq!(g.admit(&turn("Stop", "t3")), Verdict::Admit);
    }

    #[test]
    fn guard_admits_a_stop_that_beats_its_prompt_past_an_older_turn() {
        // Stop(t2) arrives while t1 is still the newest *prompted* turn: t2 is
        // unseen, hence newer — not "superseded".
        let mut g = TurnGuard::default();
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t1")));
        assert!(ok(&mut g, &turn("Stop", "t1")));
        assert_eq!(g.admit(&turn("Stop", "t2")), Verdict::Admit);
        assert_eq!(g.admit(&turn("UserPromptSubmit", "t2")), Verdict::AdmitThenAwaiting);
    }

    #[test]
    fn guard_prompt_without_turn_id_clears_the_stale_current_turn() {
        let mut g = TurnGuard::default();
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t1")));
        assert!(ok(&mut g, &turn("Stop", "t1")));
        assert!(ok(&mut g, &payload("UserPromptSubmit"))); // no turn_id
        assert!(ok(&mut g, &turn("PostToolUse", "t2")));
        assert!(ok(&mut g, &turn("Stop", "t2")), "must not read as superseded by t1");
    }

    #[test]
    fn guard_remembers_stopped_and_superseded_turns_across_turns() {
        let mut g = TurnGuard::default();
        for t in ["t1", "t2"] {
            assert!(ok(&mut g, &turn("UserPromptSubmit", t)));
            assert!(ok(&mut g, &turn("Stop", t)));
        }
        // t1 stopped two turns ago — its straggler is still recognised.
        assert_eq!(g.admit(&turn("PostToolUse", "t1")), Verdict::Drop);
        assert_eq!(g.admit(&turn("PermissionRequest", "t1")), Verdict::Drop);

        // A superseded turn (t3, never saw its Stop in time) is closed too…
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t3")));
        assert!(ok(&mut g, &turn("UserPromptSubmit", "t4")));
        assert_eq!(g.admit(&turn("PermissionRequest", "t3")), Verdict::Drop);
        assert_eq!(g.admit(&turn("Stop", "t3")), Verdict::Drop);
        assert_eq!(g.admit(&turn("PostToolUse", "t3")), Verdict::Drop);
        // …without disturbing the turn that is actually running.
        assert_eq!(g.admit(&turn("PostToolUse", "t4")), Verdict::Admit);
        assert_eq!(g.admit(&turn("Stop", "t4")), Verdict::Admit);
    }

    #[test]
    fn awaiting_followup_is_the_same_row_without_text() {
        let mut p = turn("UserPromptSubmit", "t1");
        p.prompt = Some("ship it".into());
        let prompt = codex_hook_to_event(&p, &HashMap::new(), NOW, true).unwrap();
        assert_eq!(prompt.detail.as_deref(), Some("ship it"));
        let follow = awaiting_followup(&prompt);
        assert_eq!(follow.kind, WorkKind::AwaitingInput);
        assert_eq!(follow.detail, None);
        assert_eq!(
            (follow.surface, &follow.project, &follow.source, &follow.ts),
            (prompt.surface, &prompt.project, &prompt.source, &prompt.ts)
        );
    }

    #[test]
    fn guard_memory_is_bounded() {
        let mut g = TurnGuard::default();
        for i in 0..(TurnGuard::MAX_SESSIONS * 3) {
            let mut p = turn("UserPromptSubmit", "t1");
            p.session_id = Some(format!("thr_{i}"));
            assert!(ok(&mut g, &p));
            assert!(g.sessions.len() <= TurnGuard::MAX_SESSIONS);
        }
        // …and so is each session's turn history.
        for i in 0..(TurnGuard::MAX_TURNS * 3) {
            assert!(ok(&mut g, &turn("UserPromptSubmit", &format!("t{i}"))));
        }
        assert!(g.sessions["thr_123"].turns.len() <= TurnGuard::MAX_TURNS);
    }

    fn map(p: &CodexHookPayload) -> Option<WorkEvent> {
        codex_hook_to_event(p, &HashMap::new(), NOW, false)
    }

    #[test]
    fn lifecycle_events_map_to_work_kinds() {
        for (event, kind) in [
            ("SessionStart", WorkKind::SessionStart),
            ("UserPromptSubmit", WorkKind::Prompt),
            ("PostToolUse", WorkKind::ToolUse),
            ("PermissionRequest", WorkKind::AwaitingInput),
            ("Stop", WorkKind::AwaitingInput),
            ("SessionEnd", WorkKind::SessionEnd),
        ] {
            let ev = map(&payload(event)).unwrap_or_else(|| panic!("{event} must map"));
            assert_eq!(ev.kind, kind, "{event}");
            assert_eq!(ev.surface, Surface::Codex);
            assert_eq!(ev.project.as_deref(), Some("whence"));
            assert_eq!(ev.source.as_deref(), Some("thr_123"));
            assert_eq!(ev.confidence, 1.0);
            assert_eq!(ev.ts, NOW);
        }
    }

    #[test]
    fn other_events_are_ignored() {
        for event in [
            "PreToolUse",
            "PreCompact",
            "PostCompact",
            "SubagentStart",
            "SubagentStop",
            "Interrupt",
            "PreModelRequest",
            "PostModelRequest",
            "Notification",
            "SomethingNew",
        ] {
            assert!(map(&payload(event)).is_none(), "{event} must be dropped");
        }
    }

    #[test]
    fn sessions_are_distinct_sources() {
        let a = map(&payload("UserPromptSubmit")).unwrap();
        let mut other = payload("UserPromptSubmit");
        other.session_id = Some("thr_456".into());
        let b = map(&other).unwrap();
        assert_ne!(a.source, b.source);
        // No session id → folds onto the surface's single row rather than being dropped.
        other.session_id = None;
        assert_eq!(map(&other).unwrap().source, None);
    }

    #[test]
    fn project_resolves_from_cwd() {
        let mut p = payload("Stop");
        // Windows-native cwd, mixed case and spaces → the shared slug form.
        p.cwd = Some(r"C:\Users\u\Projects\One Domino Square\".into());
        assert_eq!(map(&p).unwrap().project.as_deref(), Some("one-domino-square"));
        // A Claude-style managed worktree collapses to its repo.
        p.cwd = Some("/Users/u/Projects/whence/.claude/worktrees/zippy-graham".into());
        assert_eq!(map(&p).unwrap().project.as_deref(), Some("whence"));
        // No cwd, or one that slugs to nothing → no event at all.
        p.cwd = None;
        assert!(map(&p).is_none());
        p.cwd = Some("/".into());
        assert!(map(&p).is_none());
    }

    #[test]
    fn alias_overrides_cwd_basename() {
        let aliases = HashMap::from([("glue-mac".to_string(), "glue".to_string())]);
        let mut p = payload("UserPromptSubmit");
        p.cwd = Some("/Users/u/Projects/glue-mac".into());
        let ev = codex_hook_to_event(&p, &aliases, NOW, false).unwrap();
        assert_eq!(ev.project.as_deref(), Some("glue"));
    }

    #[test]
    fn prompt_snippet_captured_only_when_gated_on() {
        let mut p = payload("UserPromptSubmit");
        p.prompt = Some("fix the   parser\n".into());
        assert_eq!(map(&p).unwrap().detail, None, "gate off → prompt dropped here");
        let on = codex_hook_to_event(&p, &HashMap::new(), NOW, true).unwrap();
        assert_eq!(on.detail.as_deref(), Some(crate::context::sanitize("fix the   parser\n").as_str()));
        // Harness-injected turns never become moments.
        p.prompt = Some("<task-notification>done</task-notification>".into());
        assert_eq!(codex_hook_to_event(&p, &HashMap::new(), NOW, true).unwrap().detail, None);
        // Only prompts carry detail, even with the gate on.
        let mut stop = payload("Stop");
        stop.prompt = Some("should never appear".into());
        assert_eq!(codex_hook_to_event(&stop, &HashMap::new(), NOW, true).unwrap().detail, None);
    }

    #[test]
    fn tool_and_assistant_text_never_reach_the_event() {
        // A realistic full payload: the content-bearing fields are not in the
        // struct, so nothing downstream can carry them — gate on or off.
        let body = r#"{
            "session_id": "thr_123", "turn_id": "turn_9", "cwd": "/w/whence",
            "hook_event_name": "PostToolUse", "model": "gpt-x", "permission_mode": "default",
            "transcript_path": "/w/.codex/rollout.jsonl",
            "tool_name": "Bash", "tool_use_id": "t1",
            "tool_input": {"command": "cat SECRET_INPUT"},
            "tool_response": "SECRET_OUTPUT",
            "last_assistant_message": "SECRET_REPLY"
        }"#;
        let p: CodexHookPayload = serde_json::from_str(body).unwrap();
        let ev = codex_hook_to_event(&p, &HashMap::new(), NOW, true).unwrap();
        assert_eq!(ev.kind, WorkKind::ToolUse);
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains("SECRET"), "content leaked into the event: {json}");
        assert!(!format!("{p:?}").contains("SECRET"), "content retained on the payload");
    }

    #[test]
    fn payload_tolerates_missing_fields() {
        let p: CodexHookPayload = serde_json::from_str(r#"{"hook_event_name":"Stop"}"#).unwrap();
        assert!(map(&p).is_none(), "no cwd → nothing to attribute");
        assert!(serde_json::from_str::<CodexHookPayload>(r#"{"cwd":"/w/x"}"#).is_err());
    }
}
