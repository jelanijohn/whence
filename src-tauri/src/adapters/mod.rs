//! Surface adapters + the normalized `WorkEvent` they all emit.
//!
//! An adapter is per-surface and pure-ish: it translates a surface's native
//! signal (a transcript line, an Ollama poll, a shell cwd) into a `WorkEvent` and
//! sends it on the core channel. Adding a surface = adding an adapter; the engine
//! and outputs never change. The model mirrors `src/lib/types.ts` field-for-field.

pub mod claude_code;
pub mod hooks;
pub mod ollama;
pub mod terminal;

use serde::{Deserialize, Serialize};

/// Which work surface produced an event. Serializes kebab-case to match the TS
/// union (`"claude-code"`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Surface {
    ClaudeCode,
    Ollama,
    Terminal,
    ClaudeDesktop,
}

/// The kind of activity. Serializes snake_case to match the TS union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    SessionStart,
    Prompt,
    ToolUse,
    AwaitingInput,
    Active,
    Idle,
    SessionEnd,
}

impl WorkKind {
    /// Does this kind count as *focus evidence* (feeds segmentation), or is it a
    /// pure status signal? `awaiting_input`/`idle` are status-only — they never
    /// move a focus-block boundary (§7 status-vs-focus split).
    pub fn is_focus_evidence(self) -> bool {
        matches!(
            self,
            WorkKind::SessionStart | WorkKind::Prompt | WorkKind::ToolUse | WorkKind::Active
        )
    }
}

/// The single normalized shape every adapter emits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkEvent {
    /// ISO-8601 timestamp.
    pub ts: String,
    pub surface: Surface,
    /// Resolved project slug; `None` = unattributed activity.
    pub project: Option<String>,
    pub kind: WorkKind,
    /// 0..1 — cwd-derived ~1.0, temporal-correlation ~0.4.
    pub confidence: f64,
    /// Optional semantic payload (prompt summary, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Confidence at or above this is *primary* evidence (can originate a focus);
/// below it the event is merely *corroborating* (§5.3). The split tracks the
/// spec's own weighting (§6): cwd-derived attribution ~1.0, temporal/terminal
/// hints ~0.4. The engine never lets corroborating evidence open a block from
/// idle or drive a switch — see `engine::segment`.
pub const CORROBORATION_CONFIDENCE: f64 = 0.5;

impl WorkEvent {
    /// Parse `ts` to unix seconds. `None` if it isn't valid RFC-3339 — the engine
    /// drops such events rather than guessing a time.
    pub fn ts_secs(&self) -> Option<i64> {
        chrono::DateTime::parse_from_rfc3339(&self.ts)
            .ok()
            .map(|dt| dt.timestamp())
    }

    /// Is this *corroborating* evidence (a low-confidence hint) rather than a
    /// *primary* signal? Corroborating evidence may only reinforce the current
    /// focus — it never originates one (no block from idle, no switch). A bare
    /// `cd` into a repo is not, on its own, focus; treating it as such is the
    /// OS-cwd-scraping flavor principle #1 bans. §5.3: "tiebreaker / corroborator,
    /// not a primary signal."
    pub fn is_corroborating(&self) -> bool {
        self.confidence < CORROBORATION_CONFIDENCE
    }
}

/// Resolve a project **slug** from a Claude Code transcript directory name.
///
/// Claude Code encodes the session `cwd` into the directory under
/// `~/.claude/projects/` by replacing path separators with `-`
/// (e.g. `/root/Projects/waid` → `-root-Projects-waid`). The slug is the trailing
/// path segment (`waid`) — the same file-stem convention WAID uses for
/// `waid:brief=<slug>`. Paths whose final directory contains a `-` (e.g.
/// `one-domino-square`) resolve wrong here; that's what the user-editable alias
/// map is for (see settings). Pure — unit-tested.
pub fn slug_from_transcript_dir(dir_name: &str) -> Option<String> {
    let trimmed = dir_name.trim_matches('-');
    let last = trimmed.rsplit('-').next()?;
    if last.is_empty() {
        None
    } else {
        Some(last.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_takes_trailing_segment() {
        assert_eq!(slug_from_transcript_dir("-root-Projects-waid").as_deref(), Some("waid"));
        assert_eq!(slug_from_transcript_dir("-home-jelani-code-whence").as_deref(), Some("whence"));
        assert_eq!(slug_from_transcript_dir("").as_deref(), None);
        // Known lossy case the alias map covers.
        assert_eq!(slug_from_transcript_dir("-root-one-domino-square").as_deref(), Some("square"));
    }

    #[test]
    fn work_kind_focus_vs_status() {
        assert!(WorkKind::Prompt.is_focus_evidence());
        assert!(WorkKind::Active.is_focus_evidence());
        assert!(!WorkKind::AwaitingInput.is_focus_evidence());
        assert!(!WorkKind::Idle.is_focus_evidence());
    }

    #[test]
    fn ts_parses_rfc3339() {
        let e = WorkEvent {
            ts: "2026-06-24T15:00:00Z".into(),
            surface: Surface::ClaudeCode,
            project: Some("waid".into()),
            kind: WorkKind::Prompt,
            confidence: 1.0,
            detail: None,
        };
        // 2026-06-24T15:00:00Z in unix seconds.
        assert_eq!(e.ts_secs(), Some(1_782_313_200));
        // Garbage timestamps are dropped, not guessed.
        let bad = WorkEvent { ts: "not-a-time".into(), ..e.clone() };
        assert_eq!(bad.ts_secs(), None);
    }
}
