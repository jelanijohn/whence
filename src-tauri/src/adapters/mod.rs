//! Surface adapters + the normalized `WorkEvent` they all emit.
//!
//! An adapter is per-surface and pure-ish: it translates a surface's native
//! signal (a transcript line, an Ollama poll, a shell cwd) into a `WorkEvent` and
//! sends it on the core channel. Adding a surface = adding an adapter; the engine
//! and outputs never change. The model mirrors `src/lib/types.ts` field-for-field.

pub mod browser;
pub mod browser_map;
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
    /// Browser-hosted AI chat (claude.ai, chatgpt.com, …), via the first-party
    /// Whence extension POSTing to the loopback receiver. See `adapters/browser.rs`.
    Browser,
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
    /// Stable per-instance source key *within* `(project, surface)` — the Claude Code
    /// session UUID, a browser conversation URL, a terminal id. It's what lets one
    /// project show three concurrent sessions as three source rows (§9) instead of one
    /// blurred line. `None` when the surface can't tell its instances apart (a terminal
    /// with no id, Ollama liveness); then all that surface's activity on the project
    /// folds into a single source. Opaque to the engine — only equality/grouping matter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Human label for the source row (`"chatgpt web"`, `"claude web"`). `None` → the
    /// engine falls back to the surface name (`"terminal"`, `"claude code"`) and numbers
    /// duplicates. Only the browser adapter sets this today (to carry the provider, which
    /// the bare `Browser` surface can't).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    pub kind: WorkKind,
    /// 0..1 — cwd-derived ~1.0, temporal-correlation ~0.4.
    pub confidence: f64,
    /// Optional semantic payload (prompt summary, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl WorkEvent {
    /// Parse `ts` to unix seconds. `None` if it isn't valid RFC-3339 — the engine
    /// drops such events rather than guessing a time.
    ///
    /// The *primary-vs-corroborating* split (whether a low-confidence hint may only
    /// reinforce the current focus, never originate one) is **the engine's call**, not
    /// the event's: it lives in `engine::segment` against the configurable
    /// `corroborator_confidence_cutoff` (§7), so a bare `cd` (the OS-scraping flavor §1
    /// bans) can never open or switch a block. Adapters just emit honest confidences
    /// (§6: cwd-derived ~1.0, terminal/temporal hints ~0.4).
    pub fn ts_secs(&self) -> Option<i64> {
        chrono::DateTime::parse_from_rfc3339(&self.ts)
            .ok()
            .map(|dt| dt.timestamp())
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
    slugify(last)
}

/// The **shared** slug primitive (§5/§9) — the load-bearing convergence decision.
///
/// Every slug in the W-family is normalized the same way so that identical names
/// produce identical slugs, regardless of where the name came from. A project named
/// "Whence" in the browser and a `~/Projects/whence` directory on disk therefore
/// resolve to the *same* slug (`whence`) and converge onto one node automatically —
/// no merge step. Both the filesystem resolvers (here and in `claude_code`) and the
/// browser mint path (`browser_map::mint`) call this, so the guarantee holds across
/// surfaces.
///
/// Lowercase; every run of non-alphanumeric characters collapses to a single `-`;
/// leading/trailing `-` trimmed. `None` for a name that slugs to nothing (empty, or
/// all-punctuation) — an empty slug is never a valid join key. Pure — unit-tested.
///
/// Idempotent on names that are already slug-shaped (`waid` → `waid`,
/// `one-domino-square` → `one-domino-square`), so routing the existing fs basenames
/// through it doesn't change any established attribution.
pub fn slugify(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.trim().chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    (!slug.is_empty()).then_some(slug)
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
    fn slugify_normalizes_and_converges() {
        // Already slug-shaped names are unchanged (idempotent) — so the fs basenames
        // that flow through it keep resolving exactly as before.
        assert_eq!(slugify("waid").as_deref(), Some("waid"));
        assert_eq!(slugify("one-domino-square").as_deref(), Some("one-domino-square"));
        // The convergence guarantee: a browser project "Whence" and an fs dir "whence"
        // both land on the same slug.
        assert_eq!(slugify("Whence").as_deref(), Some("whence"));
        // Spaces, punctuation, and case all collapse to a single canonical form.
        assert_eq!(slugify("One Domino Square").as_deref(), Some("one-domino-square"));
        assert_eq!(slugify("  My  Project!! ").as_deref(), Some("my-project"));
        assert_eq!(slugify("glue/mac").as_deref(), Some("glue-mac"));
        // Nothing to slug → None, never an empty join key.
        assert_eq!(slugify(""), None);
        assert_eq!(slugify("  ---  "), None);
        assert_eq!(slugify("!!!"), None);
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
            source: None,
            source_label: None,
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
