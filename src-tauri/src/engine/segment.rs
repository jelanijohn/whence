//! The segmentation engine — **pure and fixture-tested**. This is where Whence
//! earns its name: capture is easy, deciding *when a switch actually happened* is
//! the intelligence. A 30-second glance at another repo is not a context switch;
//! an hour is. Get this wrong and the timeline reads "47 flickers today" instead
//! of "3 real blocks".
//!
//! No I/O, no clock reads, no `now` of its own — every time comes in via the
//! event `ts` or the explicit `now` passed to [`Segmenter::tick`]. That keeps it
//! deterministic: feed it a `WorkEvent` sequence, assert the blocks.
//!
//! **Multi-session (per project).** Whence tracks every live project at once — one
//! `SessionState` row per project — but keeps a *single* `active` project for
//! attribution. The widget shows all rows (each with its own status + timer); only
//! the active project drives the NeuroSkill label and the `timeline.jsonl` blocks,
//! so the persisted attribution stays single-stream and non-overlapping.
//!
//! Two ways the active focus moves:
//! * A **`UserPromptSubmit`** (`WorkKind::Prompt`) is explicit intent — it flips
//!   `active` to that project **immediately**, no debounce. (Hooks only.)
//! * **Transcript evidence** (`Active`/`SessionStart`/`ToolUse`) goes through the
//!   debounce (sustained evidence over `switch_min_seconds`) — the fallback when
//!   hooks aren't installed, so no-hooks behavior doesn't regress.
//!
//! The §7 status-vs-focus split lives here:
//! * **Focus switches** of the active project go through the rules above.
//! * **Status changes** (`active`/`awaiting_input`/`idle`) land on a session row
//!   immediately and never move a block boundary.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::adapters::{WorkEvent, WorkKind};

/// Tunables (surfaced read-only in settings for v1; calibrate on real data).
#[derive(Debug, Clone, Copy)]
pub struct SegmentConfig {
    /// Sustained seconds of evidence on a *new* project before a transcript-driven
    /// switch is confirmed. Start ~60–120s. (A `Prompt` bypasses this.)
    pub switch_min_seconds: i64,
    /// No qualifying events for this long ends the active block (status → idle; the
    /// gap is attributed to no project). Start ~300–600s.
    pub idle_timeout_seconds: i64,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            switch_min_seconds: 90,
            idle_timeout_seconds: 360,
        }
    }
}

/// How long past going idle a row lingers (as a quiet "idle" row) before it's
/// dropped entirely, as a multiple of `idle_timeout_seconds`. A session goes idle
/// at `last_activity + idle_timeout`, then disappears at `last_activity +
/// MULT * idle_timeout` — so a momentarily-quiet session still shows, but a project
/// you've truly left doesn't accumulate as a stale row forever. (A `SessionEnd`
/// hook removes the row at once, regardless.)
const IDLE_ROW_LINGER_MULT: i64 = 2;

/// Immediate, low-flicker status — surfaced to the widget the instant it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    AwaitingInput,
    Idle,
}

/// A closed focus block — the unit written to the timeline on close. Attribution is
/// single-stream: only the *active* project's blocks are persisted, so blocks never
/// overlap in time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FocusBlock {
    pub project: String,
    pub start: i64,
    pub end: i64,
    #[serde(rename = "eventCount")]
    pub event_count: u32,
    #[serde(rename = "meanConfidence")]
    pub mean_confidence: f64,
}

/// One live session row, as rendered by the widget. One per project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub project: String,
    pub status: Status,
    /// When this session's row/block began — drives the row timer. Unix seconds.
    #[serde(rename = "blockStart")]
    pub block_start: Option<i64>,
    /// Is this the single focused project (drives the NeuroSkill label + timeline)?
    pub active: bool,
}

/// What the widget renders right now: every live session. Empty = idle / nothing
/// attributed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FocusSnapshot {
    pub sessions: Vec<SessionSnapshot>,
}

/// Side effects the orchestrator acts on: write NeuroSkill labels, persist blocks,
/// push the snapshot to the widget.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// The displayed sessions changed (a row appeared, a status flipped, a row
    /// dropped) without a block boundary moving — push the snapshot, no side effect.
    SnapshotDirty,
    /// A focus block opened on `project` at `start` — write the `:start` label.
    BlockOpened { project: String, start: i64 },
    /// A focus block closed — write the `:end` label and append to the timeline.
    BlockClosed(FocusBlock),
}

/// Per-project display state: when the row started, when it last showed life, and
/// its current status. Attribution accounting (event counts) lives on the
/// `Segmenter` for the single active project, not here — non-active rows are
/// display-only.
#[derive(Debug, Clone)]
struct SessionState {
    start: i64,
    last_activity: i64,
    status: Status,
}

/// Evidence accumulating for a project that *might* become the new active focus via
/// the transcript-evidence debounce (a `Prompt` bypasses this entirely).
#[derive(Debug, Clone)]
struct Candidate {
    project: String,
    since: i64,
    event_count: u32,
    confidence_sum: f64,
}

/// The stateful segmenter. Drive it with [`Segmenter::ingest`] per event and
/// [`Segmenter::tick`] periodically (for idle, which is time- not event-driven).
pub struct Segmenter {
    config: SegmentConfig,
    /// Every live project, keyed by slug. `BTreeMap` → stable (alphabetical) order.
    sessions: BTreeMap<String, SessionState>,
    /// The single focused project, if any — the attribution target.
    active: Option<String>,
    /// When `active` became the focused project; the open block spans
    /// `active_since..(active session's last_activity)`.
    active_since: Option<i64>,
    /// Attribution accounting for the open block (the active project only).
    active_event_count: u32,
    active_confidence_sum: f64,
    /// Transcript-evidence debounce for switching `active`.
    candidate: Option<Candidate>,
}

impl Segmenter {
    pub fn new(config: SegmentConfig) -> Self {
        Self {
            config,
            sessions: BTreeMap::new(),
            active: None,
            active_since: None,
            active_event_count: 0,
            active_confidence_sum: 0.0,
            candidate: None,
        }
    }

    pub fn snapshot(&self) -> FocusSnapshot {
        let active = self.active.as_deref();
        FocusSnapshot {
            sessions: self
                .sessions
                .iter()
                .map(|(project, s)| SessionSnapshot {
                    project: project.clone(),
                    status: s.status,
                    block_start: Some(s.start),
                    active: active == Some(project.as_str()),
                })
                .collect(),
        }
    }

    /// Feed one event. Returns the effects it produced, in order.
    pub fn ingest(&mut self, ev: &WorkEvent) -> Vec<Effect> {
        let Some(ts) = ev.ts_secs() else {
            return Vec::new(); // un-timestamped events are dropped, not guessed.
        };
        let Some(project) = ev.project.clone() else {
            return Vec::new(); // unattributed activity can't route to a session.
        };
        let mut effects = Vec::new();
        let mut dirty = false;

        // SessionEnd closes the row outright (and the block, if it was active).
        if matches!(ev.kind, WorkKind::SessionEnd) {
            if self.active.as_deref() == Some(project.as_str()) {
                if let Some(closed) = self.close_active() {
                    effects.push(closed);
                }
            }
            if self.sessions.remove(&project).is_some() {
                dirty = true;
            }
            self.clear_candidate_for(&project);
            return finish(effects, dirty);
        }

        // Corroborating evidence (a terminal `cd`, any low-confidence hint) may only
        // *reinforce* an existing matching session — it never originates a row (a bare
        // cwd isn't focus, §1/§5.3), never switches `active`, and never starts or
        // advances a switch candidate. When it matches an existing session it refreshes
        // it (and the active block's evidence, if that's the one it matched).
        if ev.is_corroborating() {
            if let Some(s) = self.sessions.get_mut(&project) {
                s.last_activity = ts;
                if s.status == Status::Idle {
                    s.status = Status::Active; // a weak nudge lifts idle, never overrides awaiting.
                    dirty = true;
                }
                if self.active.as_deref() == Some(project.as_str()) {
                    self.active_event_count += 1;
                    self.active_confidence_sum += ev.confidence;
                }
            }
            return finish(effects, dirty);
        }

        // 1. Status surfaces immediately onto the project's row (the §7 split). Only
        //    *live* signals create a row; an `Idle` for a project we don't track is a
        //    no-op (nothing to mark idle).
        let live_signal = matches!(
            ev.kind,
            WorkKind::SessionStart
                | WorkKind::Prompt
                | WorkKind::ToolUse
                | WorkKind::Active
                | WorkKind::AwaitingInput
        );
        let desired = match ev.kind {
            WorkKind::AwaitingInput => Some(Status::AwaitingInput),
            WorkKind::Idle => Some(Status::Idle),
            WorkKind::Prompt | WorkKind::ToolUse | WorkKind::SessionStart => Some(Status::Active),
            // Generic activity is a *weak* active signal: it must NOT override
            // `awaiting_input`. When a Claude Code turn ends, the `Stop` hook sets
            // awaiting_input, then the trailing end-of-turn transcript write lands a
            // beat later as a generic `Active`; without this guard it clobbers
            // awaiting_input straight back to active and "awaiting you" is never seen.
            WorkKind::Active => match self.sessions.get(&project) {
                Some(s) if s.status == Status::AwaitingInput => None,
                _ => Some(Status::Active),
            },
            WorkKind::SessionEnd => unreachable!("handled above"),
        };

        if live_signal {
            match self.sessions.get_mut(&project) {
                Some(s) => {
                    s.last_activity = ts;
                    if let Some(st) = desired {
                        if s.status != st {
                            s.status = st;
                            dirty = true;
                        }
                    }
                }
                None => {
                    self.sessions.insert(
                        project.clone(),
                        SessionState {
                            start: ts,
                            last_activity: ts,
                            status: desired.unwrap_or(Status::Active),
                        },
                    );
                    dirty = true;
                }
            }
        } else if let Some(s) = self.sessions.get_mut(&project) {
            // Idle for a tracked project: mark it idle, but don't refresh activity
            // (idle means *no* activity — let the idle-row timeout run).
            if let Some(st) = desired {
                if s.status != st {
                    s.status = st;
                    dirty = true;
                }
            }
        }

        // 2. Status-only signals stop here — they never move a block boundary.
        if !ev.kind.is_focus_evidence() {
            return finish(effects, dirty);
        }

        // 3. Focus evidence drives the single `active` pointer.
        match self.active.clone() {
            // Already focused on this project — extend the open block's evidence.
            Some(a) if a == project => {
                self.active_event_count += 1;
                self.active_confidence_sum += ev.confidence;
                self.candidate = None;
            }
            // Nothing focused — open immediately (first real evidence wins; the
            // debounce only guards *switching away* from an established focus).
            None => {
                effects.push(self.open_active(&project, ts, 1, ev.confidence));
                self.candidate = None;
            }
            // A different project is focused.
            Some(_) => {
                if matches!(ev.kind, WorkKind::Prompt) {
                    // Explicit intent — switch immediately, no debounce.
                    if let Some(closed) = self.close_active() {
                        effects.push(closed);
                    }
                    effects.push(self.open_active(&project, ts, 1, ev.confidence));
                    self.candidate = None;
                } else {
                    // Transcript evidence — accumulate a candidate; confirm only once
                    // it's been sustained for `switch_min_seconds`.
                    match &mut self.candidate {
                        Some(c) if c.project == project => {
                            c.event_count += 1;
                            c.confidence_sum += ev.confidence;
                        }
                        _ => {
                            self.candidate = Some(Candidate {
                                project: project.clone(),
                                since: ts,
                                event_count: 1,
                                confidence_sum: ev.confidence,
                            });
                        }
                    }
                    let c = self.candidate.as_ref().unwrap();
                    if ts - c.since >= self.config.switch_min_seconds {
                        let (since, count, conf) = (c.since, c.event_count, c.confidence_sum);
                        if let Some(closed) = self.close_active() {
                            effects.push(closed);
                        }
                        // Back-date the new block to the candidate's first event so it
                        // captures the full stretch on the new project.
                        effects.push(self.open_active(&project, since, count, conf));
                        self.candidate = None;
                    }
                }
            }
        }

        finish(effects, dirty)
    }

    /// Time-driven check: close the active block if it's been idle past the timeout,
    /// mark quiet rows idle, and drop rows that have lingered idle. Call periodically
    /// (e.g. every 30s) with the current wall clock.
    pub fn tick(&mut self, now: i64) -> Vec<Effect> {
        let mut effects = Vec::new();
        let mut dirty = false;
        let idle = self.config.idle_timeout_seconds;

        // Close the active block once the active session goes idle (its evidence
        // stops). The row itself stays (now a quiet "idle" row) until it's dropped.
        if let Some(active) = self.active.clone() {
            let stale = self
                .sessions
                .get(&active)
                .map(|s| now - s.last_activity >= idle)
                .unwrap_or(true);
            if stale {
                if let Some(closed) = self.close_active() {
                    effects.push(closed);
                }
            }
        }

        // Per-row idle/drop sweep.
        let mut to_drop = Vec::new();
        for (project, s) in self.sessions.iter_mut() {
            let quiet = now - s.last_activity;
            if quiet >= idle * IDLE_ROW_LINGER_MULT {
                to_drop.push(project.clone());
            } else if quiet >= idle && s.status != Status::Idle {
                s.status = Status::Idle;
                dirty = true;
            }
        }
        for project in to_drop {
            self.sessions.remove(&project);
            self.clear_candidate_for(&project);
            dirty = true;
        }

        finish(effects, dirty)
    }

    /// Make `project` the active focus: record the start and seed the block's
    /// evidence accounting. Returns the `BlockOpened` effect.
    fn open_active(&mut self, project: &str, since: i64, count: u32, confidence_sum: f64) -> Effect {
        self.active = Some(project.to_string());
        self.active_since = Some(since);
        self.active_event_count = count;
        self.active_confidence_sum = confidence_sum;
        Effect::BlockOpened {
            project: project.to_string(),
            start: since,
        }
    }

    /// Close the open block (if any), ending it at the active session's last
    /// activity (not the call time). Clears the active pointer + accounting. Returns
    /// the `BlockClosed` effect, or `None` if nothing was active.
    fn close_active(&mut self) -> Option<Effect> {
        let project = self.active.take()?;
        let start = self.active_since.take().unwrap_or(0);
        let end = self
            .sessions
            .get(&project)
            .map(|s| s.last_activity)
            .unwrap_or(start);
        let block = FocusBlock {
            project,
            start,
            end,
            event_count: self.active_event_count,
            mean_confidence: if self.active_event_count > 0 {
                self.active_confidence_sum / self.active_event_count as f64
            } else {
                0.0
            },
        };
        self.active_event_count = 0;
        self.active_confidence_sum = 0.0;
        Some(Effect::BlockClosed(block))
    }

    fn clear_candidate_for(&mut self, project: &str) {
        if self.candidate.as_ref().map(|c| c.project == project).unwrap_or(false) {
            self.candidate = None;
        }
    }
}

/// Ensure a no-op-but-display-changing ingest still pushes a snapshot: if nothing
/// emitted a block effect yet the visible sessions changed, emit `SnapshotDirty`.
fn finish(mut effects: Vec<Effect>, dirty: bool) -> Vec<Effect> {
    if dirty && effects.is_empty() {
        effects.push(Effect::SnapshotDirty);
    }
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::Surface;

    /// Build a focus-evidence event at `secs` for `project` (confidence 1.0).
    fn ev(secs: i64, project: &str) -> WorkEvent {
        WorkEvent {
            ts: iso(secs),
            surface: Surface::ClaudeCode,
            project: Some(project.to_string()),
            kind: WorkKind::Active,
            confidence: 1.0,
            detail: None,
        }
    }

    fn status_ev(secs: i64, project: &str, kind: WorkKind) -> WorkEvent {
        WorkEvent {
            ts: iso(secs),
            surface: Surface::ClaudeCode,
            project: Some(project.to_string()),
            kind,
            confidence: 1.0,
            detail: None,
        }
    }

    fn iso(secs: i64) -> String {
        use chrono::{TimeZone, Utc};
        Utc.timestamp_opt(secs, 0).single().unwrap().to_rfc3339()
    }

    fn opened(effects: &[Effect]) -> Vec<&str> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::BlockOpened { project, .. } => Some(project.as_str()),
                _ => None,
            })
            .collect()
    }

    fn closed(effects: &[Effect]) -> Vec<FocusBlock> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::BlockClosed(b) => Some(b.clone()),
                _ => None,
            })
            .collect()
    }

    /// The active project in the snapshot, if any.
    fn active(s: &Segmenter) -> Option<String> {
        s.snapshot()
            .sessions
            .into_iter()
            .find(|x| x.active)
            .map(|x| x.project)
    }

    /// Status of a given project's row, if it exists.
    fn status_of(s: &Segmenter, project: &str) -> Option<Status> {
        s.snapshot()
            .sessions
            .into_iter()
            .find(|x| x.project == project)
            .map(|x| x.status)
    }

    fn projects(s: &Segmenter) -> Vec<String> {
        s.snapshot().sessions.into_iter().map(|x| x.project).collect()
    }

    #[test]
    fn single_project_is_one_open_block() {
        let mut s = Segmenter::new(SegmentConfig::default());
        let mut all = Vec::new();
        for t in (0..600).step_by(30) {
            all.extend(s.ingest(&ev(t, "waid")));
        }
        // One open on waid, no closes.
        assert_eq!(opened(&all), vec!["waid"]);
        assert!(closed(&all).is_empty());
        assert_eq!(active(&s).as_deref(), Some("waid"));
        assert_eq!(projects(&s), vec!["waid"]);
    }

    #[test]
    fn brief_transcript_glance_does_not_switch() {
        // 90s switch threshold. A 30s peek (transcript Active) at another repo, then
        // back to waid. The peek shows as its own row but never steals the active focus.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        s.ingest(&ev(30, "waid"));
        s.ingest(&ev(60, "whoami")); // glance... (now a row, candidate)
        s.ingest(&ev(80, "whoami")); // ...still under 90s of sustained evidence
        let back = s.ingest(&ev(100, "waid")); // back to waid clears the candidate
        // Never switched: still focused on waid, no block closed.
        assert_eq!(active(&s).as_deref(), Some("waid"));
        assert!(closed(&back).is_empty());
        // Both projects are visible rows, though.
        assert_eq!(projects(&s), vec!["waid", "whoami"]);
    }

    #[test]
    fn sustained_transcript_evidence_confirms_switch() {
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        s.ingest(&ev(30, "waid"));
        s.ingest(&ev(60, "whoami")); // candidate starts at t=60
        s.ingest(&ev(120, "whoami"));
        let confirm = s.ingest(&ev(155, "whoami")); // 155-60 = 95s >= 90 → switch
        let blocks = closed(&confirm);
        assert_eq!(blocks.len(), 1);
        // Old block: waid, ended at its last activity (t=30), not the switch time.
        assert_eq!(blocks[0].project, "waid");
        assert_eq!(blocks[0].start, 0);
        assert_eq!(blocks[0].end, 30);
        // New block opened on whoami, back-dated to the first candidate event.
        assert_eq!(opened(&confirm), vec!["whoami"]);
        assert_eq!(active(&s).as_deref(), Some("whoami"));
    }

    #[test]
    fn prompt_flips_active_immediately() {
        // A UserPromptSubmit is explicit intent — it switches the active project with
        // no debounce, even though the glance was only seconds long.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        s.ingest(&ev(30, "waid"));
        let switch = s.ingest(&status_ev(40, "whoami", WorkKind::Prompt));
        // Old waid block closed at its last activity; new whoami block opened at t=40.
        let blocks = closed(&switch);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].project, "waid");
        assert_eq!(blocks[0].end, 30);
        assert_eq!(opened(&switch), vec!["whoami"]);
        assert_eq!(active(&s).as_deref(), Some("whoami"));
        // waid is still a visible row, just no longer active.
        assert_eq!(status_of(&s, "waid"), Some(Status::Active));
        assert_eq!(projects(&s), vec!["waid", "whoami"]);
    }

    #[test]
    fn two_projects_show_with_independent_status() {
        // whoami is the active focus (you just prompted there) while waid sits awaiting
        // your input from its finished turn — both visible, each with its own status.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid")); // waid active
        s.ingest(&status_ev(10, "waid", WorkKind::AwaitingInput)); // waid turn ends
        s.ingest(&status_ev(20, "whoami", WorkKind::Prompt)); // you start whoami
        assert_eq!(active(&s).as_deref(), Some("whoami"));
        assert_eq!(status_of(&s, "waid"), Some(Status::AwaitingInput));
        assert_eq!(status_of(&s, "whoami"), Some(Status::Active));
    }

    #[test]
    fn status_only_event_pushes_snapshot() {
        // A Stop hook with no block change still must emit something so the
        // orchestrator re-pushes the snapshot (the row's status flipped).
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        let awaiting = s.ingest(&status_ev(10, "waid", WorkKind::AwaitingInput));
        assert_eq!(awaiting, vec![Effect::SnapshotDirty]);
        assert_eq!(status_of(&s, "waid"), Some(Status::AwaitingInput));
    }

    #[test]
    fn idle_timeout_closes_active_block_then_drops_row() {
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        s.ingest(&ev(30, "waid"));
        // No events for > 360s. A tick past the timeout closes the block...
        let eff = s.tick(30 + 361);
        let blocks = closed(&eff);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].project, "waid");
        assert_eq!(blocks[0].end, 30); // ended at last activity, not tick time
        assert_eq!(active(&s), None);
        // ...the row lingers as a quiet "idle" row...
        assert_eq!(status_of(&s, "waid"), Some(Status::Idle));
        // ...then drops once it's lingered past MULT * idle_timeout.
        s.tick(30 + 2 * 360 + 1);
        assert!(projects(&s).is_empty());
        // A tick before the timeout does nothing.
        let mut s2 = Segmenter::new(SegmentConfig::default());
        s2.ingest(&ev(0, "waid"));
        assert!(s2.tick(100).is_empty());
    }

    #[test]
    fn active_idle_out_leaves_other_rows() {
        // waid is active and idles out; whoami (a separate live session) keeps its row.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        s.ingest(&status_ev(350, "whoami", WorkKind::AwaitingInput)); // whoami row, awaiting
        let eff = s.tick(400); // waid idle (400-0 >= 360); whoami quiet only 50s
        assert_eq!(closed(&eff).len(), 1);
        assert_eq!(closed(&eff)[0].project, "waid");
        assert_eq!(active(&s), None);
        assert_eq!(status_of(&s, "whoami"), Some(Status::AwaitingInput));
    }

    #[test]
    fn session_end_drops_row_and_closes_block() {
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        let end = s.ingest(&status_ev(30, "waid", WorkKind::SessionEnd));
        assert_eq!(closed(&end).len(), 1);
        assert_eq!(closed(&end)[0].project, "waid");
        assert!(projects(&s).is_empty());
        assert_eq!(active(&s), None);
    }

    #[test]
    fn status_surfaces_immediately_without_moving_blocks() {
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid")); // Active + open block
        let awaiting = s.ingest(&status_ev(10, "waid", WorkKind::AwaitingInput));
        // Status flipped immediately; no block opened/closed by it.
        assert!(closed(&awaiting).is_empty() && opened(&awaiting).is_empty());
        assert_eq!(status_of(&s, "waid"), Some(Status::AwaitingInput));
        assert_eq!(active(&s).as_deref(), Some("waid"));
        // A real reply (a prompt) resumes Active — the legitimate way out of awaiting.
        let resume = s.ingest(&status_ev(20, "waid", WorkKind::Prompt));
        assert!(closed(&resume).is_empty());
        assert_eq!(status_of(&s, "waid"), Some(Status::Active));
    }

    #[test]
    fn awaiting_input_survives_trailing_transcript_write() {
        // The end-of-turn race (captured live): the Stop hook sets awaiting_input, then
        // the turn's final transcript write lands ~100ms later as a generic `Active`. It
        // must NOT clobber awaiting back to active.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "whence")); // Active + open block
        s.ingest(&status_ev(10, "whence", WorkKind::AwaitingInput));
        assert_eq!(status_of(&s, "whence"), Some(Status::AwaitingInput));

        // Trailing transcript write (generic Active) — suppressed for status.
        let trailing = s.ingest(&ev(10, "whence"));
        assert_eq!(status_of(&s, "whence"), Some(Status::AwaitingInput));
        // …but it still extends the open block (focus path is unaffected).
        assert!(closed(&trailing).is_empty());
        assert_eq!(active(&s).as_deref(), Some("whence"));

        // The next real prompt is what clears awaiting.
        s.ingest(&status_ev(30, "whence", WorkKind::Prompt));
        assert_eq!(status_of(&s, "whence"), Some(Status::Active));
    }

    /// A low-confidence corroborating event (a terminal `cd`), like the terminal
    /// adapter emits.
    fn corrob_ev(secs: i64, project: &str) -> WorkEvent {
        WorkEvent {
            ts: iso(secs),
            surface: Surface::Terminal,
            project: Some(project.to_string()),
            kind: WorkKind::Active,
            confidence: 0.4,
            detail: None,
        }
    }

    #[test]
    fn corroborating_evidence_never_opens_a_row_from_idle() {
        // A `cd` into a repo with no prior focus must NOT light up a row — a bare cwd is
        // not, on its own, focus (§1/§5.3).
        let mut s = Segmenter::new(SegmentConfig::default());
        let eff = s.ingest(&corrob_ev(0, "waid"));
        assert!(eff.is_empty());
        assert!(projects(&s).is_empty());
        assert_eq!(active(&s), None);
    }

    #[test]
    fn corroborating_evidence_extends_matching_active_block() {
        // On waid (active), then only terminal `cd`s within waid for a long stretch. The
        // corroboration keeps the block alive past what idle_timeout would close.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        s.ingest(&corrob_ev(300, "waid"));
        // A tick just after the original last_activity+timeout would have closed it, but
        // corroboration at t=300 pushed last_activity forward.
        assert!(s.tick(361).is_empty(), "corroboration should hold the block open");
        assert_eq!(active(&s).as_deref(), Some("waid"));
        // The corroborating event is folded into the block's evidence (dragging the mean
        // confidence down, honestly reflecting the weaker signal).
        let closed_eff = s.tick(300 + 361);
        let b = &closed(&closed_eff)[0];
        assert_eq!(b.event_count, 2);
        assert!((b.mean_confidence - 0.7).abs() < 1e-9); // (1.0 + 0.4) / 2
    }

    #[test]
    fn corroborating_evidence_never_drives_a_switch() {
        // On waid (active). A terminal sitting in another repo fires `cd`s well past
        // switch_min_seconds — focus must stay on waid (low-confidence can't switch), and
        // the stray repo never even becomes a row.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        for t in (30..400).step_by(30) {
            let eff = s.ingest(&corrob_ev(t, "whoami"));
            assert!(closed(&eff).is_empty() && opened(&eff).is_empty());
        }
        assert_eq!(active(&s).as_deref(), Some("waid"));
        assert_eq!(projects(&s), vec!["waid"]);
    }

    #[test]
    fn corroborating_evidence_does_not_cancel_a_real_candidate() {
        // A genuine switch is building (transcript evidence on whoami). A terminal `cd`
        // back in waid must not veto it — primary signal stays in charge.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        s.ingest(&ev(60, "whoami")); // candidate starts at 60
        s.ingest(&corrob_ev(90, "waid")); // weak nudge back — should not reset candidate
        let confirm = s.ingest(&ev(155, "whoami")); // 155-60 = 95 >= 90 → switch confirms
        assert_eq!(opened(&confirm), vec!["whoami"]);
        assert_eq!(active(&s).as_deref(), Some("whoami"));
    }

    #[test]
    fn mean_confidence_is_averaged() {
        let mut s = Segmenter::new(SegmentConfig::default());
        let mut e1 = ev(0, "waid");
        e1.confidence = 1.0;
        let mut e2 = ev(30, "waid");
        e2.confidence = 0.4;
        s.ingest(&e1);
        s.ingest(&e2);
        let eff = s.tick(30 + 361);
        let b = &closed(&eff)[0];
        assert_eq!(b.event_count, 2);
        assert!((b.mean_confidence - 0.7).abs() < 1e-9);
    }
}
