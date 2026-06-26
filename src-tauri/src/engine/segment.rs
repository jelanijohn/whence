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
//! The §7 status-vs-focus split lives here:
//! * **Focus switches** go through a debounce (sustained evidence over a window).
//! * **Status changes** (`active`/`awaiting_input`/`idle`) surface immediately and
//!   never move a block boundary.

use serde::{Deserialize, Serialize};

use crate::adapters::{WorkEvent, WorkKind};

/// Tunables (surfaced read-only in settings for v1; calibrate on real data).
#[derive(Debug, Clone, Copy)]
pub struct SegmentConfig {
    /// Sustained seconds of evidence on a *new* project before the switch is
    /// confirmed. Start ~60–120s.
    pub switch_min_seconds: i64,
    /// No qualifying events for this long ends the current block (status → idle;
    /// the gap is attributed to no project). Start ~300–600s.
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

/// Immediate, low-flicker status — surfaced to the widget the instant it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    AwaitingInput,
    Idle,
}

/// A closed focus block — the unit written to the timeline on close.
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

/// What the widget renders right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FocusSnapshot {
    pub project: Option<String>,
    pub status: Status,
    #[serde(rename = "blockStart")]
    pub block_start: Option<i64>,
}

/// Side effects the orchestrator acts on: write NeuroSkill labels, persist blocks,
/// push the snapshot to the widget.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Status changed — push to the widget immediately. No label, no block change.
    StatusChanged(Status),
    /// A focus block opened on `project` at `start` — write the `:start` label.
    BlockOpened { project: String, start: i64 },
    /// A focus block closed — write the `:end` label and append to the timeline.
    BlockClosed(FocusBlock),
}

#[derive(Debug, Clone)]
struct OpenBlock {
    project: String,
    start: i64,
    last_activity: i64,
    event_count: u32,
    confidence_sum: f64,
}

impl OpenBlock {
    fn close(&self, end: i64) -> FocusBlock {
        FocusBlock {
            project: self.project.clone(),
            start: self.start,
            end,
            event_count: self.event_count,
            mean_confidence: if self.event_count > 0 {
                self.confidence_sum / self.event_count as f64
            } else {
                0.0
            },
        }
    }
}

/// Evidence accumulating for a project that *might* become the new current focus.
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
    current: Option<OpenBlock>,
    candidate: Option<Candidate>,
    status: Status,
}

impl Segmenter {
    pub fn new(config: SegmentConfig) -> Self {
        Self {
            config,
            current: None,
            candidate: None,
            status: Status::Idle,
        }
    }

    pub fn snapshot(&self) -> FocusSnapshot {
        FocusSnapshot {
            project: self.current.as_ref().map(|b| b.project.clone()),
            status: self.status,
            block_start: self.current.as_ref().map(|b| b.start),
        }
    }

    /// Feed one event. Returns the effects it produced, in order.
    pub fn ingest(&mut self, ev: &WorkEvent) -> Vec<Effect> {
        let Some(ts) = ev.ts_secs() else {
            return Vec::new(); // un-timestamped events are dropped, not guessed.
        };
        let mut effects = Vec::new();

        // 1. Status surfaces immediately, independent of focus (the §7 split).
        let new_status = match ev.kind {
            WorkKind::AwaitingInput => Some(Status::AwaitingInput),
            WorkKind::Idle | WorkKind::SessionEnd => Some(Status::Idle),
            // Explicit work signals (a prompt, a tool call, a session opening) always
            // mean active — they're how `awaiting_input` is cleared by a real reply.
            WorkKind::Prompt | WorkKind::ToolUse | WorkKind::SessionStart => Some(Status::Active),
            // Generic activity — a transcript "file changed" or Ollama liveness — is a
            // *weak* active signal: it lifts `idle` back to `active`, but must NOT
            // override `awaiting_input`. When a Claude Code turn ends, the `Stop` hook
            // (fast HTTP) sets `awaiting_input`, then the trailing end-of-turn
            // transcript write lands a beat later (~100ms, filesystem-watch latency)
            // as a generic `Active`; without this guard it clobbers `awaiting_input`
            // straight back to `active` and the "awaiting you" state is never seen.
            WorkKind::Active if self.status == Status::AwaitingInput => None,
            WorkKind::Active => Some(Status::Active),
        };
        if let Some(s) = new_status {
            if s != self.status {
                self.status = s;
                effects.push(Effect::StatusChanged(s));
            }
        }

        // 2. Focus evidence drives the debounce. Status-only events stop here —
        //    they never move a block boundary.
        if !ev.kind.is_focus_evidence() {
            return effects;
        }
        let Some(project) = ev.project.clone() else {
            return effects; // unattributed activity can't open/extend a block.
        };

        match &mut self.current {
            // No current focus — open immediately (first real evidence wins; the
            // debounce only guards *switching away* from an established focus).
            None => {
                self.candidate = None;
                self.current = Some(OpenBlock {
                    project: project.clone(),
                    start: ts,
                    last_activity: ts,
                    event_count: 1,
                    confidence_sum: ev.confidence,
                });
                effects.push(Effect::BlockOpened { project, start: ts });
            }
            // Same project — extend, and clear any competing candidate.
            Some(cur) if cur.project == project => {
                cur.last_activity = ts;
                cur.event_count += 1;
                cur.confidence_sum += ev.confidence;
                self.candidate = None;
            }
            // Different project — accumulate candidate evidence; confirm only once
            // it's been sustained for `switch_min_seconds`.
            Some(cur) => {
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
                    // Confirmed switch: close the old block at its last activity,
                    // open the new one back-dated to the candidate's first event so
                    // the block captures the full stretch on the new project.
                    let closed = cur.close(cur.last_activity);
                    let new = OpenBlock {
                        project: c.project.clone(),
                        start: c.since,
                        last_activity: ts,
                        event_count: c.event_count,
                        confidence_sum: c.confidence_sum,
                    };
                    effects.push(Effect::BlockClosed(closed));
                    effects.push(Effect::BlockOpened {
                        project: new.project.clone(),
                        start: new.start,
                    });
                    self.current = Some(new);
                    self.candidate = None;
                }
            }
        }

        effects
    }

    /// Time-driven check: close the current block if it's been idle past the
    /// timeout. Call periodically (e.g. every 30s) with the current wall clock.
    pub fn tick(&mut self, now: i64) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Some(cur) = &self.current {
            if now - cur.last_activity >= self.config.idle_timeout_seconds {
                let closed = cur.close(cur.last_activity);
                self.current = None;
                self.candidate = None;
                effects.push(Effect::BlockClosed(closed));
                if self.status != Status::Idle {
                    self.status = Status::Idle;
                    effects.push(Effect::StatusChanged(Status::Idle));
                }
            }
        }
        effects
    }
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
        assert_eq!(s.snapshot().project.as_deref(), Some("waid"));
    }

    #[test]
    fn brief_glance_does_not_switch() {
        // 90s switch threshold. A 30s peek at another repo, then back to waid.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        s.ingest(&ev(30, "waid"));
        s.ingest(&ev(60, "whoami")); // glance...
        s.ingest(&ev(80, "whoami")); // ...still under 90s of sustained evidence
        let back = s.ingest(&ev(100, "waid")); // back to waid clears the candidate
        // Never switched: still on waid, no block closed.
        assert_eq!(s.snapshot().project.as_deref(), Some("waid"));
        assert!(closed(&back).is_empty());
    }

    #[test]
    fn sustained_evidence_confirms_switch() {
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
        assert_eq!(s.snapshot().project.as_deref(), Some("whoami"));
        assert_eq!(s.snapshot().block_start, Some(60));
    }

    #[test]
    fn idle_timeout_closes_block() {
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        s.ingest(&ev(30, "waid"));
        // No events for > 360s. tick at t=30+361.
        let eff = s.tick(30 + 361);
        let blocks = closed(&eff);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].project, "waid");
        assert_eq!(blocks[0].end, 30); // ended at last activity, not tick time
        assert_eq!(s.snapshot().project, None);
        assert_eq!(s.snapshot().status, Status::Idle);
        // A tick before the timeout does nothing.
        let mut s2 = Segmenter::new(SegmentConfig::default());
        s2.ingest(&ev(0, "waid"));
        assert!(s2.tick(100).is_empty());
    }

    #[test]
    fn status_surfaces_immediately_without_moving_blocks() {
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid")); // Active + open block
        let awaiting = s.ingest(&status_ev(10, "waid", WorkKind::AwaitingInput));
        // Status flipped immediately; no block opened/closed by it.
        assert_eq!(awaiting, vec![Effect::StatusChanged(Status::AwaitingInput)]);
        assert_eq!(s.snapshot().status, Status::AwaitingInput);
        assert_eq!(s.snapshot().project.as_deref(), Some("waid"));
        assert_eq!(s.snapshot().block_start, Some(0));
        // A real reply (a prompt) resumes Active — the legitimate way out of awaiting.
        let resume = s.ingest(&status_ev(20, "waid", WorkKind::Prompt));
        assert!(resume.contains(&Effect::StatusChanged(Status::Active)));
        assert!(closed(&resume).is_empty());
    }

    #[test]
    fn awaiting_input_survives_trailing_transcript_write() {
        // The end-of-turn race (captured live): the Stop hook sets awaiting_input,
        // then the turn's final transcript write lands ~100ms later as a generic
        // `Active`. It must NOT clobber awaiting back to active.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "whence")); // Active + open block
        let awaiting = s.ingest(&status_ev(10, "whence", WorkKind::AwaitingInput));
        assert_eq!(awaiting, vec![Effect::StatusChanged(Status::AwaitingInput)]);

        // Trailing transcript write (generic Active) — suppressed for status.
        let trailing = s.ingest(&ev(10, "whence"));
        assert!(!trailing.iter().any(|e| matches!(e, Effect::StatusChanged(_))));
        assert_eq!(s.snapshot().status, Status::AwaitingInput);
        // …but it still extends the open block (focus path is unaffected).
        assert!(closed(&trailing).is_empty());
        assert_eq!(s.snapshot().block_start, Some(0));

        // The next real prompt is what clears awaiting.
        let reply = s.ingest(&status_ev(30, "whence", WorkKind::Prompt));
        assert!(reply.contains(&Effect::StatusChanged(Status::Active)));
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
