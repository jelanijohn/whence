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
//! **Per-project registry (§7/§9).** Whence tracks every live project at once as a
//! [`ProjectState`], and within each, every live **source** (Claude Code session,
//! browser conversation, terminal) keyed by [`source_key`]. The widget renders this
//! as a roster: one row per project (status rolled up by attention priority), each
//! expandable to its sources. Attribution stays single-stream, though — a *single*
//! `active` project drives the NeuroSkill label and the `timeline.jsonl` blocks, so
//! persisted blocks never overlap. The source breakdown is display-only.
//!
//! ## The three-tier trust model (§7)
//!
//! Every focus-evidence event falls into one tier by how much it may change the
//! active answer — read off `kind` + `confidence`, no model change:
//!
//! * **You acted** — `Prompt` / `SessionStart` at full confidence. You're actually
//!   here. Strongest: opens a block, switches **immediately** (no debounce), and
//!   marks the block *present*, which *protects* it from weaker signals.
//! * **Claude worked** — autonomous transcript growth (`Active`) and the tool /
//!   subagent start+done pairs (`ToolUse`/`ToolDone`, `AgentStart`/`AgentDone`).
//!   High confidence in *which* project, low in whether you're watching. Keeps a
//!   block alive (and can open one from idle), but is *slow* to trigger a switch
//!   and *never* protects: it can only build a switch candidate once the current
//!   block has gone *running* (your last prompt aged out).
//! * **Weak hint** — anything below the corroborator confidence cutoff (terminal
//!   cwd, Ollama temporal correlation). Only nudges: reinforces whatever's already
//!   current. It can't open or originate a focus — but on a *running* block a stray
//!   hint pointing at another repo is evidence you've physically moved on, so it
//!   **drops** the running block (it still never *opens* the weak project).
//!
//! ## Present vs running (§7)
//!
//! The active block tracks when you last *acted* (last `Prompt`/`SessionStart`)
//! separately from when the project last saw *any* activity:
//!
//! * **present** while you've acted within the attention-recency window — you're here;
//! * **running** once only autonomous activity has arrived since — Claude's still
//!   going, but you may have stepped away.
//!
//! Either way the block stays open and keeps accruing time (autonomous work is real
//! work). What changes is how hard it holds: a *present* block makes any competing
//! autonomous signal earn the full debounce (and is fully protected from weak hints);
//! a *running* block yields easily — a single weak hint elsewhere drops it. The
//! widget shows which it is (`waid · active` vs `waid · running`).
//!
//! The §7 status-vs-focus split also lives here:
//! * **Focus switches** of the active project go through the rules above.
//! * **Status changes** (`active`/`awaiting_input`/`idle`) land on a session row
//!   immediately and never move a block boundary.
//!
//! ## Open work (busy vs quiet)
//!
//! Idle is decided from silence — but a Claude Code session is silent exactly when
//! its work is long: a 10-minute build writes nothing to the transcript until the
//! tool result lands, a long thinking stretch nothing until the block completes,
//! and an async subagent runs on after the parent turn has ended. So each source
//! keeps a small set of **open work** entries, bounded by hook pairs rather than by
//! activity: a *turn* (`Prompt` → `AwaitingInput`, Claude Code only — the browser
//! streams `Active` while it works), a *tool call* (`ToolUse` → `ToolDone`, keyed by
//! the call id so parallel calls are independent), a *subagent* (`AgentStart` →
//! `AgentDone`). While any entry is open the source is **busy** and [`tick`] judges
//! it by the much longer `busy_idle_timeout_seconds` instead of
//! `idle_timeout_seconds`. No heartbeat is needed: the entries are state. An
//! `AwaitingInput` says *why* it waits ([`WaitReason`]): a mid-turn wait (permission
//! prompt) keeps the turn and its pending tool open — the approved tool runs on with
//! no hook of its own — while a turn-end wait (`Stop`, `idle_prompt`) voids the turn
//! and any tool entry still open, since a tool that never got its done is an orphan
//! (an Escape interrupt fires neither `Stop` nor a tool-done). The next prompt
//! clears orphans too, and the busy timeout bounds whatever is left. A tool event
//! also proves a turn is in flight, so it (re)opens the turn — hooks installed
//! mid-session still cover the thinking after the tool. `SessionStart` leaves open
//! work alone: the transcript watcher emits one when the session's file appears,
//! which under a polling watcher can land *after* the hook's prompt/tool. Status
//! and busy are independent: after `Stop` with an agent still running the row reads
//! *awaiting you* yet stays off the idle path, with the agent's type as its busy
//! label.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::adapters::{Surface, WaitReason, WorkEvent, WorkKind};

/// Tunables (surfaced in settings; calibrate the debounce on real data).
#[derive(Debug, Clone, Copy)]
pub struct SegmentConfig {
    /// Sustained seconds of evidence on a *new* project before a transcript-driven
    /// switch is confirmed. Start ~60–120s. (A *you-acted* event bypasses this.)
    pub switch_min_seconds: i64,
    /// No qualifying events for this long ends the active block (status → idle; the
    /// gap is attributed to no project). Start ~300–600s.
    pub idle_timeout_seconds: i64,
    /// The silence a source with *open work* (a turn, tool call, or subagent in
    /// flight — see the module doc) is allowed before it's judged idle anyway. Much
    /// longer than `idle_timeout_seconds`: it bounds a *lost* close signal, not
    /// normal work. ~1800s.
    pub busy_idle_timeout_seconds: i64,
    /// Confidence at or above this is *primary* evidence (can open a block / drive a
    /// switch); below it the event is a *weak hint* — it only corroborates the
    /// current focus (and, on a *running* block, can drop it). ~0.6. Tracks the
    /// spec's own weighting (§6): cwd-derived ~1.0, terminal/temporal hints ~0.4.
    pub corroborator_confidence_cutoff: f64,
    /// How recently you must have *acted* (a `Prompt`/`SessionStart`) for the active
    /// block to count as *present* rather than *running* (§7 present-vs-running).
    /// ~120s.
    pub attention_recency_seconds: i64,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            switch_min_seconds: 90,
            idle_timeout_seconds: 360,
            busy_idle_timeout_seconds: 1800,
            corroborator_confidence_cutoff: 0.6,
            attention_recency_seconds: 120,
        }
    }
}

// An idle row lingers (as a quiet "idle" row) for one further `idle_timeout_seconds`
// after going idle, then is dropped — so a momentarily-quiet session still shows,
// but a project you've truly left doesn't accumulate as a stale row forever. The
// linger is measured from the idle flip (`status_since`), not from last activity,
// so a *busy* row — which idles later — can never be dropped while still active.
// (A `SessionEnd` hook removes the row at once, regardless.)

/// Immediate, low-flicker status — surfaced to the widget the instant it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    AwaitingInput,
    Idle,
}

/// Whether the active block reflects *your* presence or just autonomous work (§7).
/// Only the active row carries one; every other row's `presence` is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    /// You've acted within the attention-recency window — you're here.
    Present,
    /// Only autonomous activity since your last act — Claude's going, you may have
    /// stepped away.
    Running,
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

/// One live **source** under a project row — a single Claude Code session, browser
/// conversation, or terminal. A project with three concurrent sessions renders as
/// three of these under one [`ProjectSnapshot`] (§9), each with its own status + timer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    pub surface: Surface,
    /// Stable per-instance source id — the part of [`source_key`] after the unit
    /// separator: the browser conversation URL, the CC session UUID, the terminal id.
    /// `None` when the surface gives no id (its activity folds onto one row). The widget
    /// uses it to target a click — raising the exact browser tab — and only browser rows
    /// act on it; for everything else it's display-inert metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Display label — the adapter's `source_label` (`"chatgpt web"`) or the surface
    /// name, numbered when a project has several of the same kind (`"terminal 1"`).
    pub label: String,
    pub status: Status,
    /// When this source entered its current status — drives the source's state timer
    /// (time-in-status, §9). Unix seconds.
    #[serde(rename = "statusSince")]
    pub status_since: i64,
    /// The label of the most recently opened tool call / subagent still in flight on
    /// this source (`"Bash"`, `"Explore"`) — the widget's "still running" hint.
    /// `None` when nothing but a bare turn (or nothing at all) is open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy: Option<String>,
}

/// One project row in the roster (§9). One per project that currently has a live or
/// recently-live source. Carries the project's rolled-up status + the focus marker;
/// the per-source breakdown is in `sources` (revealed when the row is expanded).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub project: String,
    /// Attention-priority roll-up of the sources' statuses (ACTIVE > AWAITING > IDLE).
    pub status: Status,
    /// When the project entered its current rolled-up status — drives the row's
    /// state timer (time-in-status, §9). `None` only if the project has no sources.
    #[serde(rename = "statusSince")]
    pub status_since: Option<i64>,
    /// Is this the single focused project (drives the NeuroSkill label + timeline)?
    pub active: bool,
    /// Present vs running — set only on the `active` row (§7); `None` otherwise.
    pub presence: Option<Presence>,
    /// The project's live sources, ordered oldest-first (stable; for the expand view).
    pub sources: Vec<SourceSnapshot>,
}

/// What the widget renders right now: every live project as a roster row. Empty =
/// idle / nothing attributed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FocusSnapshot {
    pub projects: Vec<ProjectSnapshot>,
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

/// Per-**source** display state: one live session/conversation/terminal. Attribution
/// accounting (event counts) lives on the `Segmenter` for the single active project,
/// not here — sources are display-only.
#[derive(Debug, Clone)]
struct SourceState {
    surface: Surface,
    /// Adapter-supplied label hint (`source_label`); `None` → fall back to the surface
    /// name. Numbering of duplicates happens at snapshot time, not here.
    label: Option<String>,
    start: i64,
    last_activity: i64,
    status: Status,
    /// When the current status began — the source's time-in-status timer base.
    status_since: i64,
    /// Units of work currently open on this source (module doc "Open work"), keyed
    /// `turn` / `tool:<id>` / `agent:<id>`. Non-empty = *busy*: judged by the long
    /// busy timeout rather than the idle timeout.
    open_work: BTreeMap<String, OpenWork>,
    /// Opening-order counter for `open_work` (see [`OpenWork::seq`]).
    next_work_seq: u64,
}

/// One open unit of work on a source: its opening order and display label.
#[derive(Debug, Clone)]
struct OpenWork {
    /// Position in this source's opening order. Events carry whole seconds, so
    /// parallel tool calls dispatched together tie on time — the sequence is what
    /// says which opened *last* (the one the row shows).
    seq: u64,
    label: Option<String>,
}

/// The open-work key of a Claude Code turn (`Prompt` → `AwaitingInput`).
const TURN_KEY: &str = "turn";

/// The open-work key for a tool call / subagent: `<prefix>:<id>`, with a single
/// shared slot when the surface carried no id.
fn work_key(prefix: &str, ev: &WorkEvent) -> String {
    format!("{prefix}:{}", ev.work_id.as_deref().unwrap_or("?"))
}

impl SourceState {
    /// The un-numbered display label: the adapter hint, else the surface name.
    fn base_label(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| surface_label(self.surface).to_string())
    }

    fn is_busy(&self) -> bool {
        !self.open_work.is_empty()
    }

    /// The silence after which this source is judged idle: the long busy timeout
    /// while work is open, the normal idle timeout otherwise.
    fn idle_after(&self, cfg: &SegmentConfig) -> i64 {
        if self.is_busy() {
            cfg.busy_idle_timeout_seconds
        } else {
            cfg.idle_timeout_seconds
        }
    }

    /// The "still running" hint: the label of the most recently opened tool call or
    /// subagent. A bare turn has no label — the status dot already says *active*.
    fn busy_label(&self) -> Option<String> {
        self.open_work
            .iter()
            .filter(|(k, _)| k.as_str() != TURN_KEY)
            .max_by_key(|(_, w)| w.seq)
            .and_then(|(_, w)| w.label.clone())
    }

    /// Open/close work per the event's kind. Returns whether the displayed busy
    /// label changed (so the caller can mark the snapshot dirty).
    fn note_open_work(&mut self, ev: &WorkEvent) -> bool {
        let before = self.busy_label();
        match ev.kind {
            // A prompt opens a turn — Claude Code only: the browser emits `Active`
            // while streaming and `AwaitingInput` when done, so its turns are never
            // silent, and a tab closed mid-stream must not pin a busy row. A new turn
            // also means the previous turn's tool calls are over (a lost done can't
            // outlive the turn); agents may run across turns and close on their own.
            WorkKind::Prompt => {
                if ev.surface == Surface::ClaudeCode {
                    let turn = self.new_work(None);
                    self.open_work.insert(TURN_KEY.to_string(), turn);
                }
                self.open_work.retain(|k, _| !k.starts_with("tool:"));
            }
            // A mid-turn wait (`permission_prompt`, `elicitation_dialog`) pauses the
            // turn on you: `PreToolUse` fired *before* the prompt and the approved tool
            // then runs with no hook of its own, so the turn and the pending tool stay
            // open — they're what cover that stretch and the thinking after it. A
            // turn-end wait (`Stop`, `StopFailure`, `idle_prompt`; or a surface that
            // can't say) voids the turn and any tool still open — a tool with no done
            // by then was interrupted. Agents outlive turns and close on their own.
            WorkKind::AwaitingInput => {
                if ev.wait != Some(WaitReason::MidTurn) {
                    self.open_work.remove(TURN_KEY);
                    self.open_work.retain(|k, _| !k.starts_with("tool:"));
                }
            }
            // A tool call proves a turn is in flight — (re)open it, so a session whose
            // prompt Whence never saw (hooks installed mid-turn) is still covered
            // through the thinking after the tool. Background tools return at once
            // (their done fires on dispatch), so a done never implies a stale turn.
            WorkKind::ToolUse => {
                self.ensure_turn(ev);
                let work = self.new_work(ev.work_label.clone());
                self.open_work.insert(work_key("tool", ev), work);
            }
            WorkKind::ToolDone => {
                self.ensure_turn(ev);
                close_work(&mut self.open_work, "tool", ev);
            }
            WorkKind::AgentStart => {
                let work = self.new_work(ev.work_label.clone());
                self.open_work.insert(work_key("agent", ev), work);
            }
            WorkKind::AgentDone => close_work(&mut self.open_work, "agent", ev),
            // An explicit idle means nothing is running. `SessionStart` is *not* a
            // reset: the transcript watcher emits one when the session's file is
            // created, and under a polling watcher that lands up to a couple of seconds
            // late — after the hook's prompt or first tool — so clearing here would
            // void live work. A genuinely dead session's leftovers are bounded by the
            // busy timeout and cleared by the next prompt or turn-end wait anyway.
            WorkKind::Idle => self.open_work.clear(),
            WorkKind::SessionStart
            | WorkKind::Active
            | WorkKind::Select
            | WorkKind::SessionEnd => {}
        }
        self.busy_label() != before
    }

    /// Open the turn if it isn't already (Claude Code only — see `Prompt`). An
    /// existing turn keeps its place in the order.
    fn ensure_turn(&mut self, ev: &WorkEvent) {
        if ev.surface == Surface::ClaudeCode && !self.open_work.contains_key(TURN_KEY) {
            let turn = self.new_work(None);
            self.open_work.insert(TURN_KEY.to_string(), turn);
        }
    }

    /// Mint the next entry in opening order.
    fn new_work(&mut self, label: Option<String>) -> OpenWork {
        let seq = self.next_work_seq;
        self.next_work_seq += 1;
        OpenWork { seq, label }
    }
}

/// Close the `<prefix>:<id>` entry a done event names; a done with no id closes
/// every entry of that prefix (the surface can't say which, so none is in flight).
fn close_work(open: &mut BTreeMap<String, OpenWork>, prefix: &str, ev: &WorkEvent) {
    match ev.work_id.as_deref() {
        Some(id) => {
            open.remove(&format!("{prefix}:{id}"));
        }
        None => open.retain(|k, _| !k.starts_with(&format!("{prefix}:"))),
    }
}

/// Per-project display state: the set of live sources under one project, keyed by
/// [`source_key`]. The project's status/timer are *derived* from its sources (a
/// roll-up), so nothing here duplicates source state.
#[derive(Debug, Clone)]
struct ProjectState {
    sources: BTreeMap<String, SourceState>,
    /// The latest `last_activity` of any source since removed from `sources` (dropped
    /// after lingering idle, or ended). Sources idle on their own clocks — a quiet
    /// browser tab can be swept away while a busy Claude Code session on the same
    /// project holds the block open for 30 minutes — and the block's end must still
    /// be the project's last real evidence, not the busy survivor's older activity.
    evicted_last_activity: Option<i64>,
}

impl ProjectState {
    fn new() -> Self {
        Self { sources: BTreeMap::new(), evicted_last_activity: None }
    }

    /// The most recent activity across all sources, evicted ones included — where
    /// the project's block ends when it closes. `None` if nothing was ever seen.
    fn last_activity(&self) -> Option<i64> {
        self.sources
            .values()
            .map(|s| s.last_activity)
            .chain(self.evicted_last_activity)
            .max()
    }

    /// Remove a source, keeping its activity as evidence for the block end.
    fn evict(&mut self, key: &str) -> bool {
        match self.sources.remove(key) {
            Some(s) => {
                self.evicted_last_activity =
                    self.evicted_last_activity.max(Some(s.last_activity));
                true
            }
            None => false,
        }
    }

    /// Has every source gone quiet past its own threshold (busy sources get the
    /// long one)? The block-close test. A sourceless project is stale.
    fn is_stale(&self, now: i64, cfg: &SegmentConfig) -> bool {
        self.sources
            .values()
            .all(|s| now - s.last_activity >= s.idle_after(cfg))
    }

    /// Attention-priority roll-up: ACTIVE beats AWAITING beats IDLE (§9 ordering).
    fn rolled_status(&self) -> Status {
        self.sources
            .values()
            .map(|s| s.status)
            .max_by_key(|st| status_rank(*st))
            .unwrap_or(Status::Idle)
    }

    /// Time-in-status base for the row timer: the oldest `status_since` among the
    /// sources currently showing the rolled-up status. `None` if there are no sources.
    fn status_since_for(&self, status: Status) -> Option<i64> {
        self.sources
            .values()
            .filter(|s| s.status == status)
            .map(|s| s.status_since)
            .min()
    }

    /// Render the sources oldest-first, numbering duplicates of the same base label
    /// (`terminal 1`, `terminal 2`) so concurrent same-kind sessions read distinctly.
    fn source_snapshots(&self) -> Vec<SourceSnapshot> {
        // Carry the map key alongside each state — its id half (after the unit
        // separator) is the stable per-instance source id the widget needs to target a
        // click. The values' display order is unchanged (oldest-first, then base label).
        let mut srcs: Vec<(&String, &SourceState)> = self.sources.iter().collect();
        srcs.sort_by(|(_, a), (_, b)| {
            a.start.cmp(&b.start).then(a.base_label().cmp(&b.base_label()))
        });
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (_, s) in &srcs {
            *counts.entry(s.base_label()).or_default() += 1;
        }
        let mut seen: HashMap<String, usize> = HashMap::new();
        srcs.into_iter()
            .map(|(key, s)| {
                let base = s.base_label();
                let label = if counts[&base] > 1 {
                    let n = seen.entry(base.clone()).or_insert(0);
                    *n += 1;
                    format!("{base} {n}")
                } else {
                    base
                };
                // `source_key` is `"<tag>\u{1f}<id>"`; an empty id (surface gives none)
                // → `None`, never an empty string the frontend would treat as clickable.
                let source = key
                    .split_once('\u{1f}')
                    .map(|(_, id)| id)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string);
                SourceSnapshot {
                    surface: s.surface,
                    source,
                    label,
                    status: s.status,
                    status_since: s.status_since,
                    busy: s.busy_label(),
                }
            })
            .collect()
    }
}

/// Attention-priority rank for the status roll-up + ordering (§9): higher wins.
fn status_rank(s: Status) -> u8 {
    match s {
        Status::Active => 2,
        Status::AwaitingInput => 1,
        Status::Idle => 0,
    }
}

/// Surface → its default un-numbered source label (used when the adapter gives no
/// `source_label`). Kebab is for keying ([`source_key`]); this is the human form.
fn surface_label(s: Surface) -> &'static str {
    match s {
        Surface::ClaudeCode => "claude code",
        Surface::Ollama => "ollama",
        Surface::Terminal => "terminal",
        Surface::ClaudeDesktop => "claude desktop",
        Surface::Browser => "browser",
    }
}

/// Stable key for a source *within a project*: its surface plus the adapter's
/// per-instance `source` id (the CC session UUID, browser URL, terminal id). When the
/// surface gives no id, all its activity on a project folds onto one key (one row).
/// Uses a unit-separator the ids never contain, so distinct surfaces/ids never collide.
fn source_key(ev: &WorkEvent) -> String {
    let tag = match ev.surface {
        Surface::ClaudeCode => "claude-code",
        Surface::Ollama => "ollama",
        Surface::Terminal => "terminal",
        Surface::ClaudeDesktop => "claude-desktop",
        Surface::Browser => "browser",
    };
    format!("{tag}\u{1f}{}", ev.source.as_deref().unwrap_or(""))
}

/// Evidence accumulating for a project that *might* become the new active focus via
/// the transcript-evidence debounce (a *you-acted* event bypasses this entirely).
#[derive(Debug, Clone)]
struct Candidate {
    project: String,
    since: i64,
    event_count: u32,
    confidence_sum: f64,
}

/// The stateful segmenter. Drive it with [`Segmenter::ingest`] per event and
/// [`Segmenter::tick`] periodically (for idle + the present→running flip, both
/// time- not event-driven).
pub struct Segmenter {
    config: SegmentConfig,
    /// Every live project, keyed by slug. `BTreeMap` → stable (alphabetical) order.
    /// Each holds its own set of live sources (§9).
    projects: BTreeMap<String, ProjectState>,
    /// The single focused project, if any — the attribution target.
    active: Option<String>,
    /// When `active` became the focused project; the open block spans
    /// `active_since..(active session's last_activity)`.
    active_since: Option<i64>,
    /// When you last *acted* on the active project (`Prompt`/`SessionStart`). Drives
    /// present-vs-running. `None` = the block was opened by autonomous work and never
    /// since touched by a you-acted event → *running* from the start.
    active_last_prompt: Option<i64>,
    /// Cached present/running of the active block (recomputed on ingest + tick so the
    /// present→running flip emits a snapshot even with no new event). `None` when
    /// nothing is active.
    active_presence: Option<Presence>,
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
            projects: BTreeMap::new(),
            active: None,
            active_since: None,
            active_last_prompt: None,
            active_presence: None,
            active_event_count: 0,
            active_confidence_sum: 0.0,
            candidate: None,
        }
    }

    pub fn snapshot(&self) -> FocusSnapshot {
        let active = self.active.as_deref();
        FocusSnapshot {
            projects: self
                .projects
                .iter()
                .map(|(project, p)| {
                    let is_active = active == Some(project.as_str());
                    let status = p.rolled_status();
                    ProjectSnapshot {
                        project: project.clone(),
                        status,
                        status_since: p.status_since_for(status),
                        active: is_active,
                        presence: if is_active { self.active_presence } else { None },
                        sources: p.source_snapshots(),
                    }
                })
                .collect(),
        }
    }

    /// Is this a *weak hint* (low-confidence corroborator) rather than primary
    /// evidence? Below the cutoff a signal only reinforces — it never opens a row,
    /// originates a focus, or builds a candidate (§5.3).
    fn is_weak(&self, ev: &WorkEvent) -> bool {
        ev.confidence < self.config.corroborator_confidence_cutoff
    }

    /// Is this a *you-acted* event — explicit human intent that switches immediately
    /// and marks the block *present* (§7 top tier)? A `Prompt` (you typed), a
    /// `SessionStart` (you launched Claude here), or a `Select` (you clicked a source
    /// row to pull it into focus), at primary confidence.
    fn is_acted(&self, ev: &WorkEvent) -> bool {
        matches!(ev.kind, WorkKind::Prompt | WorkKind::SessionStart | WorkKind::Select)
            && ev.confidence >= self.config.corroborator_confidence_cutoff
    }

    /// Present/running of the active block as of `now`. `None` if nothing is active.
    fn presence_at(&self, now: i64) -> Option<Presence> {
        self.active.as_ref()?;
        match self.active_last_prompt {
            Some(p) if now - p <= self.config.attention_recency_seconds => Some(Presence::Present),
            _ => Some(Presence::Running),
        }
    }

    /// Recompute the cached presence for `now`; return whether it changed (so the
    /// caller can mark the snapshot dirty for the present→running flip).
    fn refresh_presence(&mut self, now: i64) -> bool {
        let p = self.presence_at(now);
        if p != self.active_presence {
            self.active_presence = p;
            true
        } else {
            false
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

        // SessionEnd closes the *source* that ended. The project (and its block, if
        // active) only goes away once its last source is gone — another live session on
        // the same project keeps it attributed.
        if matches!(ev.kind, WorkKind::SessionEnd) {
            let key = source_key(ev);
            let now_empty = {
                let Some(proj) = self.projects.get_mut(&project) else {
                    return finish(effects, dirty);
                };
                if proj.evict(&key) {
                    dirty = true;
                }
                proj.sources.is_empty()
            };
            if now_empty {
                // Close while the (now sourceless) project is still in the map so the
                // block ends at its last activity, then drop the project.
                if self.active.as_deref() == Some(project.as_str()) {
                    if let Some(closed) = self.close_active() {
                        effects.push(closed);
                    }
                }
                self.projects.remove(&project);
                self.clear_candidate_for(&project);
            }
            return finish(effects, dirty);
        }

        // Weak hint (corroborating evidence — a terminal `cd`, any sub-cutoff signal).
        // It may only *reinforce* an existing matching session: never originates a row
        // (a bare cwd isn't focus, §1/§5.3), never opens a focus, never starts or
        // advances a switch candidate. The one teeth it has: on a *running* block a
        // stray hint pointing at a *different* repo is evidence you've physically moved,
        // so it drops the running block (§7 "a running block yields easily"). Even then
        // it never *opens* the weak project — whatever real evidence comes next does.
        if self.is_weak(ev) {
            // A weak hint may reinforce an existing project, never *originate* one (a bare
            // cwd isn't focus, §1/§5.3). Under a project that's already attributed, though,
            // it upserts its own source row — a terminal you actually have open on that repo
            // is honest to show (§9); a `cd` into an unattributed repo still creates nothing.
            if self.projects.contains_key(&project) {
                let key = source_key(ev);
                let proj = self.projects.get_mut(&project).unwrap();
                match proj.sources.get_mut(&key) {
                    Some(s) => {
                        s.last_activity = ts;
                        if s.status == Status::Idle {
                            s.status = Status::Active; // lifts idle, never overrides awaiting.
                            s.status_since = ts;
                            dirty = true;
                        }
                    }
                    None => {
                        proj.sources.insert(
                            key,
                            SourceState {
                                surface: ev.surface,
                                label: ev.source_label.clone(),
                                start: ts,
                                last_activity: ts,
                                status: Status::Active,
                                status_since: ts,
                                open_work: BTreeMap::new(),
                                next_work_seq: 0,
                            },
                        );
                        dirty = true;
                    }
                }
                if self.active.as_deref() == Some(project.as_str()) {
                    self.active_event_count += 1;
                    self.active_confidence_sum += ev.confidence;
                }
            }
            // Running-yield: a stray hint elsewhere drops a running block.
            if self.active.is_some()
                && self.active.as_deref() != Some(project.as_str())
                && self.presence_at(ts) == Some(Presence::Running)
            {
                if let Some(closed) = self.close_active() {
                    effects.push(closed);
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
                | WorkKind::ToolDone
                | WorkKind::AgentStart
                | WorkKind::AgentDone
                | WorkKind::Active
                | WorkKind::AwaitingInput
                | WorkKind::Select
        );
        let key = source_key(ev);
        let desired = match ev.kind {
            WorkKind::AwaitingInput => Some(Status::AwaitingInput),
            WorkKind::Idle => Some(Status::Idle),
            // `AgentDone` lifts an `AwaitingInput` parent on purpose: a finished
            // background agent wakes the parent with a completion notification, which
            // Claude Code delivers through `UserPromptSubmit` (hooks.rs
            // `is_synthetic_prompt`) — a `Prompt` → `Active` follows a beat later
            // regardless. `ToolDone` likewise: the approved tool's result resumes the
            // turn (the generic `Active` guard below can't see that).
            WorkKind::Prompt
            | WorkKind::ToolUse
            | WorkKind::ToolDone
            | WorkKind::AgentStart
            | WorkKind::AgentDone
            | WorkKind::SessionStart
            | WorkKind::Select => Some(Status::Active),
            // Generic activity is a *weak* active signal — but only on Claude Code,
            // where a turn ends with the `Stop` hook setting awaiting_input and the
            // trailing end-of-turn transcript write lands a beat later as a generic
            // `Active`; without this guard that trailing write clobbers awaiting_input
            // straight back to active and "awaiting you" is never seen. Other surfaces
            // have no such trailing-write race: the browser's `Active` means the model
            // is streaming *right now* (the page shows a Stop control), so it must
            // override a stale awaiting — else a generating Design/chat session reads as
            // "awaiting you" the whole time it's thinking.
            WorkKind::Active
                if ev.surface == Surface::ClaudeCode
                    && matches!(
                        self.projects.get(&project).and_then(|p| p.sources.get(&key)),
                        Some(s) if s.status == Status::AwaitingInput,
                    ) =>
            {
                None
            }
            WorkKind::Active => Some(Status::Active),
            WorkKind::SessionEnd => unreachable!("handled above"),
        };

        if live_signal {
            let proj = self.projects.entry(project.clone()).or_insert_with(ProjectState::new);
            match proj.sources.get_mut(&key) {
                Some(s) => {
                    s.last_activity = ts;
                    dirty |= s.note_open_work(ev);
                    if let Some(st) = desired {
                        if s.status != st {
                            s.status = st;
                            s.status_since = ts;
                            dirty = true;
                        }
                    }
                }
                None => {
                    proj.sources.insert(
                        key.clone(),
                        SourceState {
                            surface: ev.surface,
                            label: ev.source_label.clone(),
                            start: ts,
                            last_activity: ts,
                            status: desired.unwrap_or(Status::Active),
                            status_since: ts,
                            open_work: BTreeMap::new(),
                            next_work_seq: 0,
                        },
                    );
                    proj.sources.get_mut(&key).unwrap().note_open_work(ev);
                    dirty = true;
                }
            }
        } else if let Some(s) =
            self.projects.get_mut(&project).and_then(|p| p.sources.get_mut(&key))
        {
            // Idle for a tracked source: mark it idle, but don't refresh activity
            // (idle means *no* activity — let the idle-row timeout run). Nothing is
            // running either, so any open work is void.
            s.note_open_work(ev);
            if let Some(st) = desired {
                if s.status != st {
                    s.status = st;
                    s.status_since = ts;
                    dirty = true;
                }
            }
        }

        // 2. Status-only signals stop here — they never move a block boundary.
        if !ev.kind.is_focus_evidence() {
            return finish(effects, dirty);
        }

        // 3. Focus evidence drives the single `active` pointer.
        let acted = self.is_acted(ev);
        match self.active.clone() {
            // Already focused on this project — extend the open block's evidence. A
            // you-acted event also refreshes presence (you're here / still here).
            Some(a) if a == project => {
                self.active_event_count += 1;
                self.active_confidence_sum += ev.confidence;
                if acted {
                    self.active_last_prompt = Some(ts);
                }
                self.candidate = None;
            }
            // Nothing focused — open immediately (first real evidence wins; the
            // debounce only guards *switching away* from an established focus).
            None => {
                effects.push(self.open_active(&project, ts, 1, ev.confidence, acted));
                self.candidate = None;
            }
            // A different project is focused.
            Some(_) => {
                if acted {
                    // You acted — explicit intent, switch immediately, no debounce.
                    if let Some(closed) = self.close_active() {
                        effects.push(closed);
                    }
                    effects.push(self.open_active(&project, ts, 1, ev.confidence, true));
                    self.candidate = None;
                } else if self.presence_at(ts) == Some(Presence::Present) {
                    // Claude worked, but the current block is *present* (you acted within
                    // the recency window) — protected. A background task elsewhere can't
                    // yank you off a project you're actively prompting on; build nothing.
                    self.candidate = None;
                } else {
                    // Claude worked while the current block is *running* — accumulate a
                    // candidate; confirm only once it's been sustained for switch_min_seconds.
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
                        effects.push(self.open_active(&project, since, count, conf, false));
                        self.candidate = None;
                    }
                }
            }
        }

        dirty |= self.refresh_presence(ts);
        finish(effects, dirty)
    }

    /// Time-driven check: close the active block if it's been idle past the timeout,
    /// flip a stale-prompt block present→running, mark quiet rows idle, and drop rows
    /// that have lingered idle. Call periodically (e.g. every 30s) with the current
    /// wall clock.
    pub fn tick(&mut self, now: i64) -> Vec<Effect> {
        let mut effects = Vec::new();
        let mut dirty = false;
        let cfg = self.config;

        // Close the active block once every source of the active project has gone
        // quiet past its own threshold (a busy source — open turn/tool/agent — gets
        // the long one). The row itself stays (now a quiet "idle" row) until dropped.
        if let Some(active) = self.active.clone() {
            let stale = self
                .projects
                .get(&active)
                .map(|p| p.is_stale(now, &cfg))
                .unwrap_or(true);
            if stale {
                if let Some(closed) = self.close_active() {
                    effects.push(closed);
                }
            }
        }

        // Per-source idle/drop sweep: each source idles (then drops) on its own clock; a
        // project disappears once its last source is gone.
        let mut empty_projects = Vec::new();
        for (slug, proj) in self.projects.iter_mut() {
            let mut drop_sources = Vec::new();
            for (key, s) in proj.sources.iter_mut() {
                if s.status == Status::Idle {
                    // Linger one idle timeout past the idle flip, then drop.
                    if now - s.status_since >= cfg.idle_timeout_seconds {
                        drop_sources.push(key.clone());
                    }
                } else if now - s.last_activity >= s.idle_after(&cfg) {
                    // Silence won — whatever work was open is dead, not running.
                    s.status = Status::Idle;
                    s.status_since = now;
                    s.open_work.clear();
                    dirty = true;
                }
            }
            for key in drop_sources {
                proj.evict(&key);
                dirty = true;
            }
            if proj.sources.is_empty() {
                empty_projects.push(slug.clone());
            }
        }
        for slug in empty_projects {
            self.projects.remove(&slug);
            self.clear_candidate_for(&slug);
            dirty = true;
        }

        // The present→running flip is time-driven too — surface it to the widget.
        if self.refresh_presence(now) {
            dirty = true;
        }

        finish(effects, dirty)
    }

    /// Make `project` the active focus: record the start, seed the block's evidence
    /// accounting, and set presence per `acted` (a you-acted open is *present*; an
    /// autonomous open is *running* from the start). Returns the `BlockOpened` effect.
    fn open_active(
        &mut self,
        project: &str,
        since: i64,
        count: u32,
        confidence_sum: f64,
        acted: bool,
    ) -> Effect {
        self.active = Some(project.to_string());
        self.active_since = Some(since);
        self.active_last_prompt = if acted { Some(since) } else { None };
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
        let end = self.project_last_activity(&project).unwrap_or(start);
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
        self.active_last_prompt = None;
        self.active_presence = None;
        Some(Effect::BlockClosed(block))
    }

    /// Most recent activity across a project's sources — the project's liveness for the
    /// idle/block-close logic. `None` if the project isn't tracked (or has no sources).
    fn project_last_activity(&self, project: &str) -> Option<i64> {
        self.projects.get(project).and_then(|p| p.last_activity())
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

    /// Build a focus-evidence event at `secs` for `project` (confidence 1.0). This is
    /// *autonomous* transcript growth (`Active`) — a "Claude worked" signal.
    fn ev(secs: i64, project: &str) -> WorkEvent {
        WorkEvent {
            ts: iso(secs),
            surface: Surface::ClaudeCode,
            project: Some(project.to_string()),
            source: None,
            source_label: None,
            kind: WorkKind::Active,
            confidence: 1.0,
            detail: None,
            work_id: None,
            work_label: None,
            wait: None,
        }
    }

    fn status_ev(secs: i64, project: &str, kind: WorkKind) -> WorkEvent {
        WorkEvent {
            ts: iso(secs),
            surface: Surface::ClaudeCode,
            project: Some(project.to_string()),
            source: None,
            source_label: None,
            kind,
            confidence: 1.0,
            detail: None,
            work_id: None,
            work_label: None,
            wait: None,
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
            .projects
            .into_iter()
            .find(|x| x.active)
            .map(|x| x.project)
    }

    /// Rolled-up status of a given project's row, if it exists.
    fn status_of(s: &Segmenter, project: &str) -> Option<Status> {
        s.snapshot()
            .projects
            .into_iter()
            .find(|x| x.project == project)
            .map(|x| x.status)
    }

    /// Presence of a given project's row (only the active row carries one).
    fn presence_of(s: &Segmenter, project: &str) -> Option<Presence> {
        s.snapshot()
            .projects
            .into_iter()
            .find(|x| x.project == project)
            .and_then(|x| x.presence)
    }

    fn projects(s: &Segmenter) -> Vec<String> {
        s.snapshot().projects.into_iter().map(|x| x.project).collect()
    }

    /// The source labels under a given project's row, in snapshot order.
    fn source_labels(s: &Segmenter, project: &str) -> Vec<String> {
        s.snapshot()
            .projects
            .into_iter()
            .find(|x| x.project == project)
            .map(|x| x.sources.into_iter().map(|src| src.label).collect())
            .unwrap_or_default()
    }

    /// An open-work start/done event (`ToolUse`/`ToolDone`/`AgentStart`/`AgentDone`)
    /// on the default (un-keyed) Claude Code source of `project`.
    fn work_ev(
        secs: i64,
        project: &str,
        kind: WorkKind,
        id: Option<&str>,
        label: Option<&str>,
    ) -> WorkEvent {
        WorkEvent {
            work_id: id.map(str::to_string),
            work_label: label.map(str::to_string),
            ..status_ev(secs, project, kind)
        }
    }

    /// An `AwaitingInput` with its reason — what the hooks adapter emits.
    fn wait_ev(secs: i64, project: &str, wait: WaitReason) -> WorkEvent {
        WorkEvent { wait: Some(wait), ..status_ev(secs, project, WorkKind::AwaitingInput) }
    }

    /// The busy label of a project's first source row, if any.
    fn busy_of(s: &Segmenter, project: &str) -> Option<String> {
        s.snapshot()
            .projects
            .into_iter()
            .find(|x| x.project == project)
            .and_then(|x| x.sources.into_iter().next())
            .and_then(|src| src.busy)
    }

    // ---- open work: a silent session with a turn / tool / agent in flight is busy,
    // not idle (module doc "Open work").

    #[test]
    fn open_turn_holds_status_and_block_past_idle_timeout() {
        // You prompted; Claude is thinking / running a long tool. Nothing is written
        // for 400s — well past the 360s idle timeout — yet the turn is still open.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&status_ev(0, "waid", WorkKind::Prompt));
        // (The tick still reports the present→running flip — only block effects matter.)
        assert!(closed(&s.tick(400)).is_empty());
        assert_eq!(status_of(&s, "waid"), Some(Status::Active));
        assert_eq!(active(&s).as_deref(), Some("waid"));
        assert_eq!(busy_of(&s, "waid"), None); // a bare turn carries no label
        // Only the long busy timeout ends it — a lost `Stop` can't pin the row forever.
        let eff = s.tick(1801);
        assert_eq!(closed(&eff).len(), 1);
        assert_eq!(closed(&eff)[0].end, 0);
        assert_eq!(status_of(&s, "waid"), Some(Status::Idle));
        assert_eq!(active(&s), None);
    }

    #[test]
    fn stop_closes_the_turn_so_normal_idle_resumes() {
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&status_ev(0, "waid", WorkKind::Prompt));
        s.ingest(&status_ev(10, "waid", WorkKind::AwaitingInput)); // Stop
        let eff = s.tick(400);
        assert_eq!(closed(&eff).len(), 1);
        assert_eq!(closed(&eff)[0].end, 10);
        assert_eq!(status_of(&s, "waid"), Some(Status::Idle));
    }

    #[test]
    fn open_tool_survives_a_permission_wait() {
        // PreToolUse fires *before* the permission prompt; after you approve, the
        // long tool runs with no hook of its own. The open tool entry is what keeps
        // the row off the idle path through that silence.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&status_ev(0, "waid", WorkKind::Prompt));
        s.ingest(&work_ev(5, "waid", WorkKind::ToolUse, Some("a"), Some("Bash")));
        s.ingest(&wait_ev(6, "waid", WaitReason::MidTurn)); // permission_prompt
        assert_eq!(status_of(&s, "waid"), Some(Status::AwaitingInput));
        assert!(closed(&s.tick(400)).is_empty()); // approved + running: silent, not idle
        assert_eq!(active(&s).as_deref(), Some("waid"));
        assert_eq!(busy_of(&s, "waid").as_deref(), Some("Bash"));
        // The tool finishes: active again, label gone — and the turn is still open
        // (no Stop yet), so a long silent think after the result doesn't idle either.
        let done = s.ingest(&work_ev(500, "waid", WorkKind::ToolDone, Some("a"), None));
        assert_eq!(done, vec![Effect::SnapshotDirty]);
        assert_eq!(status_of(&s, "waid"), Some(Status::Active));
        assert_eq!(busy_of(&s, "waid"), None);
        assert!(closed(&s.tick(900)).is_empty());
        assert_eq!(status_of(&s, "waid"), Some(Status::Active));
        // Stop; normal idle resumes.
        s.ingest(&wait_ev(1000, "waid", WaitReason::TurnEnd));
        let eff = s.tick(1400);
        assert_eq!(closed(&eff)[0].end, 1000);
        assert_eq!(status_of(&s, "waid"), Some(Status::Idle));
    }

    #[test]
    fn turn_end_wait_clears_an_orphaned_tool_but_not_an_agent() {
        // Escape mid-tool: no PostToolUseFailure ever comes. The idle_prompt a minute
        // later says the turn is over — the orphan goes, the normal idle path resumes.
        // An agent (async, outliving turns) is untouched by the same wait.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&status_ev(0, "waid", WorkKind::Prompt));
        s.ingest(&work_ev(5, "waid", WorkKind::ToolUse, Some("a"), Some("Bash")));
        s.ingest(&work_ev(6, "waid", WorkKind::AgentStart, Some("x"), Some("Explore")));
        s.ingest(&wait_ev(70, "waid", WaitReason::TurnEnd)); // idle_prompt
        assert_eq!(busy_of(&s, "waid").as_deref(), Some("Explore")); // Bash is gone
        s.ingest(&work_ev(80, "waid", WorkKind::AgentDone, Some("x"), None));
        assert_eq!(busy_of(&s, "waid"), None);
        let eff = s.tick(450);
        assert_eq!(closed(&eff)[0].end, 80);
        assert_eq!(status_of(&s, "waid"), Some(Status::Idle));
    }

    #[test]
    fn an_unreasoned_wait_counts_as_turn_end() {
        // A surface that can't say why it waits (the browser) ends the turn.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&status_ev(0, "waid", WorkKind::Prompt));
        s.ingest(&work_ev(5, "waid", WorkKind::ToolUse, Some("a"), Some("Bash")));
        s.ingest(&status_ev(10, "waid", WorkKind::AwaitingInput)); // wait: None
        assert_eq!(busy_of(&s, "waid"), None);
        assert_eq!(closed(&s.tick(400))[0].end, 10);
    }

    #[test]
    fn tool_events_reopen_a_turn_whence_never_saw() {
        // Hooks installed (or Whence launched) mid-turn: no Prompt arrived. The tool
        // pair still proves a turn is in flight, so the silent think after the tool's
        // result stays off the idle path until Stop.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&work_ev(0, "waid", WorkKind::ToolUse, Some("a"), Some("Read")));
        s.ingest(&work_ev(2, "waid", WorkKind::ToolDone, Some("a"), None));
        assert_eq!(busy_of(&s, "waid"), None);
        assert!(closed(&s.tick(400)).is_empty());
        assert_eq!(status_of(&s, "waid"), Some(Status::Active));
        s.ingest(&wait_ev(500, "waid", WaitReason::TurnEnd));
        assert_eq!(closed(&s.tick(900))[0].end, 500);
    }

    #[test]
    fn transcript_create_after_a_tool_start_keeps_it_busy() {
        // The transcript watcher emits SessionStart when the session's JSONL appears;
        // under a 2 s polling watcher that lands after the hook's PreToolUse on the
        // same source. It must not void the live work.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&status_ev(0, "waid", WorkKind::Prompt));
        s.ingest(&work_ev(1, "waid", WorkKind::ToolUse, Some("a"), Some("Bash")));
        s.ingest(&status_ev(2, "waid", WorkKind::SessionStart)); // file created
        assert_eq!(busy_of(&s, "waid").as_deref(), Some("Bash"));
        assert!(closed(&s.tick(400)).is_empty());
        assert_eq!(status_of(&s, "waid"), Some(Status::Active));
    }

    #[test]
    fn evicted_quiet_source_still_bounds_the_block_end() {
        // Claude Code is busy (a tool in flight since t=0, judged by the 30-minute
        // timeout); a browser tab on the same project was last active at t=600 and
        // idles on the normal clock: idle at 960, swept away at 1320. When the busy
        // source finally goes stale at 1800 the block must end at the project's last
        // real evidence (600), not at the survivor's older activity (0).
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&work_ev(0, "waid", WorkKind::ToolUse, Some("a"), Some("Bash")));
        s.ingest(&WorkEvent {
            surface: Surface::Browser,
            source: Some("https://claude.ai/chat/1".into()),
            ..ev(600, "waid")
        });
        assert!(closed(&s.tick(960)).is_empty());
        assert!(closed(&s.tick(1320)).is_empty());
        assert_eq!(s.snapshot().projects[0].sources.len(), 1); // browser row gone
        let eff = s.tick(1800);
        let block = &closed(&eff)[0];
        assert_eq!((block.start, block.end), (0, 600));
    }

    #[test]
    fn ended_source_still_bounds_the_block_end() {
        // Same for an explicit SessionEnd of a non-last source.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&work_ev(0, "waid", WorkKind::ToolUse, Some("a"), Some("Bash")));
        s.ingest(&WorkEvent { source: Some("other".into()), ..ev(300, "waid") });
        s.ingest(&WorkEvent {
            source: Some("other".into()),
            ..status_ev(310, "waid", WorkKind::SessionEnd)
        });
        assert_eq!((closed(&s.tick(1800))[0]).end, 300);
    }

    #[test]
    fn agent_keeps_source_busy_after_the_parent_turn_stops() {
        // An async agent: the parent turn ends (Stop → awaiting you) while the agent
        // works on for many minutes. Status says awaiting; busy says Explore; no idle.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&status_ev(0, "waid", WorkKind::Prompt));
        s.ingest(&work_ev(5, "waid", WorkKind::AgentStart, Some("x"), Some("Explore")));
        s.ingest(&status_ev(10, "waid", WorkKind::AwaitingInput)); // Stop
        assert!(closed(&s.tick(400)).is_empty());
        assert_eq!(status_of(&s, "waid"), Some(Status::AwaitingInput));
        assert_eq!(active(&s).as_deref(), Some("waid"));
        assert_eq!(busy_of(&s, "waid").as_deref(), Some("Explore"));
        s.ingest(&work_ev(1000, "waid", WorkKind::AgentDone, Some("x"), None));
        assert_eq!(busy_of(&s, "waid"), None);
        let eff = s.tick(1370);
        assert_eq!(closed(&eff)[0].end, 1000);
        assert_eq!(status_of(&s, "waid"), Some(Status::Idle));
    }

    #[test]
    fn parallel_tools_stay_busy_until_the_last_done() {
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&work_ev(10, "waid", WorkKind::ToolUse, Some("a"), Some("Read")));
        s.ingest(&work_ev(10, "waid", WorkKind::ToolUse, Some("b"), Some("Bash")));
        s.ingest(&work_ev(20, "waid", WorkKind::ToolDone, Some("a"), None));
        assert!(s.tick(400).is_empty()); // b still running
        assert_eq!(busy_of(&s, "waid").as_deref(), Some("Bash"));
        s.ingest(&work_ev(500, "waid", WorkKind::ToolDone, Some("b"), None));
        assert_eq!(busy_of(&s, "waid"), None);
        // (The tool pair opened a turn; Stop ends it, then normal idle resumes.)
        s.ingest(&wait_ev(510, "waid", WaitReason::TurnEnd));
        let eff = s.tick(870);
        assert_eq!(closed(&eff)[0].end, 510);
    }

    #[test]
    fn busy_timeout_bounds_a_lost_close_signal_then_row_lingers_and_drops() {
        // A tool whose done never arrives (an Escape interrupt fires none): the row
        // stays busy only until the busy timeout of *silence*, then idles, lingers
        // one idle timeout as a quiet row, and drops.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&work_ev(10, "waid", WorkKind::ToolUse, Some("a"), Some("Bash")));
        assert!(s.tick(1000).is_empty());
        assert_eq!(status_of(&s, "waid"), Some(Status::Active));
        let eff = s.tick(1811);
        assert_eq!(closed(&eff)[0].end, 10);
        assert_eq!(status_of(&s, "waid"), Some(Status::Idle));
        assert_eq!(busy_of(&s, "waid"), None); // silence voided the open work
        assert!(s.tick(1811 + 359).is_empty());
        assert_eq!(projects(&s), vec!["waid"]);
        s.tick(1811 + 360);
        assert!(projects(&s).is_empty());
    }

    #[test]
    fn prompt_clears_stale_tools_but_not_agents() {
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&work_ev(0, "waid", WorkKind::ToolUse, Some("a"), Some("Bash")));
        s.ingest(&work_ev(1, "waid", WorkKind::AgentStart, Some("x"), Some("Plan")));
        // A new turn: the old turn's tool call is over whatever we heard; the agent
        // runs across turns. Then Stop: the agent alone keeps the row busy.
        s.ingest(&status_ev(100, "waid", WorkKind::Prompt));
        s.ingest(&status_ev(110, "waid", WorkKind::AwaitingInput));
        assert_eq!(busy_of(&s, "waid").as_deref(), Some("Plan"));
        assert!(closed(&s.tick(500)).is_empty());
        s.ingest(&work_ev(600, "waid", WorkKind::AgentDone, Some("x"), None));
        assert_eq!(closed(&s.tick(961)).len(), 1);
    }

    #[test]
    fn same_second_starts_show_the_latest_not_the_highest_key() {
        // Two tool calls dispatched in one second, with ids that sort the other
        // way: the row shows the one that opened *last*, and the second start still
        // pushes a snapshot even though the row was already active.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&work_ev(10, "waid", WorkKind::ToolUse, Some("z"), Some("Bash")));
        assert_eq!(busy_of(&s, "waid").as_deref(), Some("Bash"));
        let eff = s.ingest(&work_ev(10, "waid", WorkKind::ToolUse, Some("a"), Some("Read")));
        assert_eq!(eff, vec![Effect::SnapshotDirty]);
        assert_eq!(busy_of(&s, "waid").as_deref(), Some("Read"));
        // Closing the latest falls back to the one still open, whatever its key.
        s.ingest(&work_ev(10, "waid", WorkKind::ToolDone, Some("a"), None));
        assert_eq!(busy_of(&s, "waid").as_deref(), Some("Bash"));
    }

    #[test]
    fn tool_done_without_id_clears_every_tool() {
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&work_ev(0, "waid", WorkKind::ToolUse, Some("a"), Some("Bash")));
        s.ingest(&work_ev(0, "waid", WorkKind::ToolUse, None, Some("Grep")));
        s.ingest(&work_ev(10, "waid", WorkKind::ToolDone, None, None));
        assert_eq!(busy_of(&s, "waid"), None);
        s.ingest(&wait_ev(20, "waid", WaitReason::TurnEnd));
        assert_eq!(closed(&s.tick(381)).len(), 1);
    }

    #[test]
    fn browser_prompt_opens_no_turn() {
        // The browser streams `Active` while the model works, so its turns are never
        // silent; a tab closed mid-stream must not pin a 30-minute busy row.
        let mut s = Segmenter::new(SegmentConfig::default());
        let mut p = status_ev(0, "waid", WorkKind::Prompt);
        p.surface = Surface::Browser;
        p.source = Some("https://claude.ai/chat/1".into());
        s.ingest(&p);
        assert_eq!(closed(&s.tick(361)).len(), 1);
        assert_eq!(status_of(&s, "waid"), Some(Status::Idle));
    }

    #[test]
    fn tool_start_on_an_active_row_still_pushes_a_snapshot() {
        // Status is unchanged (active → active) but the busy label appeared — the
        // widget must hear about it.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        let eff = s.ingest(&work_ev(1, "waid", WorkKind::ToolUse, Some("a"), Some("Bash")));
        assert_eq!(eff, vec![Effect::SnapshotDirty]);
        assert_eq!(busy_of(&s, "waid").as_deref(), Some("Bash"));
        // Same label again (a second Bash call) → nothing displayed changes → no push.
        let eff = s.ingest(&work_ev(2, "waid", WorkKind::ToolUse, Some("b"), Some("Bash")));
        assert!(eff.is_empty());
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
        // No prompts here — waid opened on autonomous `Active`, so it's *running* and
        // the debounce governs the switch (the no-hooks fallback path).
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
    fn session_start_in_new_project_switches_immediately() {
        // Launching Claude in another project (a new transcript → SessionStart) is an
        // explicit act — a "you acted" signal, so it switches like a prompt, no debounce.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        s.ingest(&ev(30, "waid"));
        let switch = s.ingest(&status_ev(40, "whoami", WorkKind::SessionStart));
        let blocks = closed(&switch);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].project, "waid");
        assert_eq!(opened(&switch), vec!["whoami"]);
        assert_eq!(active(&s).as_deref(), Some("whoami"));
        assert_eq!(presence_of(&s, "whoami"), Some(Presence::Present));
    }

    #[test]
    fn present_block_protected_from_autonomous_switch() {
        // You prompted waid (present). Claude then works autonomously on whoami for the
        // whole attention-recency window — a present block is protected, so no candidate
        // builds and there's no switch, no matter how much background work piles up.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&status_ev(0, "waid", WorkKind::Prompt)); // present at t=0
        // All whoami evidence within 120s of the prompt → waid stays present throughout.
        s.ingest(&ev(30, "whoami"));
        s.ingest(&ev(60, "whoami"));
        let still = s.ingest(&ev(110, "whoami"));
        assert!(closed(&still).is_empty() && opened(&still).is_empty());
        assert_eq!(active(&s).as_deref(), Some("waid"));
        assert_eq!(presence_of(&s, "waid"), Some(Presence::Present));
        // whoami is a visible row but carries no presence (only the active row does).
        assert_eq!(presence_of(&s, "whoami"), None);
    }

    #[test]
    fn running_block_yields_to_autonomous_evidence() {
        // You prompted waid at t=0, but past the attention-recency window it's *running*
        // (only autonomous activity since). Sustained whoami evidence now builds a
        // candidate and confirms a switch on the normal debounce.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&status_ev(0, "waid", WorkKind::Prompt));
        s.ingest(&ev(150, "waid")); // autonomous; past recency → waid now running
        s.ingest(&ev(200, "whoami")); // candidate starts at 200
        let confirm = s.ingest(&ev(295, "whoami")); // 295-200 = 95 >= 90 → switch
        assert_eq!(opened(&confirm), vec!["whoami"]);
        assert_eq!(active(&s).as_deref(), Some("whoami"));
    }

    #[test]
    fn present_flips_to_running_after_recency() {
        // A prompt opens a *present* block; once the attention-recency window lapses with
        // no further act, a tick flips it to *running* and pushes a snapshot.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&status_ev(0, "waid", WorkKind::Prompt));
        assert_eq!(presence_of(&s, "waid"), Some(Presence::Present));
        // Before the window lapses: still present, no snapshot churn.
        assert!(s.tick(100).is_empty());
        assert_eq!(presence_of(&s, "waid"), Some(Presence::Present));
        // Past the window (and before idle): flips to running, emits a snapshot.
        let eff = s.tick(200);
        assert_eq!(eff, vec![Effect::SnapshotDirty]);
        assert_eq!(presence_of(&s, "waid"), Some(Presence::Running));
        // Still the active block — running keeps accruing time.
        assert_eq!(active(&s).as_deref(), Some("waid"));
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

    #[test]
    fn browser_streaming_active_overrides_stale_awaiting() {
        // The trailing-write guard is Claude-Code-specific. A browser session can sit
        // awaiting (idle, ball in your court), then the model starts generating — which
        // the extension reports as a generic `Active` (a Stop control is on the page),
        // NOT as a turn-increment Prompt (the assistant turn hasn't landed yet). That
        // `Active` must flip the row out of awaiting; otherwise "thinking…" reads as
        // "awaiting you" for the whole generation (the reported bug).
        let mut s = Segmenter::new(SegmentConfig::default());

        let mut idle = status_ev(0, "whence", WorkKind::AwaitingInput);
        idle.surface = Surface::Browser;
        idle.source = Some("https://claude.ai/design/p/d1".into());
        s.ingest(&idle);
        assert_eq!(status_of(&s, "whence"), Some(Status::AwaitingInput));

        // Model starts streaming → generic Active on the SAME browser source.
        let mut streaming = status_ev(10, "whence", WorkKind::Active);
        streaming.surface = Surface::Browser;
        streaming.source = Some("https://claude.ai/design/p/d1".into());
        s.ingest(&streaming);
        assert_eq!(
            status_of(&s, "whence"),
            Some(Status::Active),
            "a browser streaming Active must override a stale awaiting"
        );
    }

    /// A low-confidence corroborating event (a terminal `cd`), like the terminal
    /// adapter emits.
    fn corrob_ev(secs: i64, project: &str) -> WorkEvent {
        WorkEvent {
            ts: iso(secs),
            surface: Surface::Terminal,
            project: Some(project.to_string()),
            source: None,
            source_label: None,
            kind: WorkKind::Active,
            confidence: 0.4,
            detail: None,
            work_id: None,
            work_label: None,
            wait: None,
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
        // corroboration keeps the *block* alive past what idle_timeout would close — the
        // project's last activity is the max across its sources, and the terminal bumps it.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        s.ingest(&corrob_ev(300, "waid"));
        // waid now has two sources: the (idling) Claude Code session and the live terminal.
        assert_eq!(source_labels(&s, "waid"), vec!["claude code", "terminal"]);
        // A tick just after the Claude Code source's own timeout: that source goes idle,
        // but the terminal at t=300 holds the block open (no close).
        let eff = s.tick(361);
        assert!(closed(&eff).is_empty(), "corroboration should hold the block open");
        assert_eq!(active(&s).as_deref(), Some("waid"));
        // The corroborating event is folded into the block's evidence (dragging the mean
        // confidence down, honestly reflecting the weaker signal).
        let closed_eff = s.tick(300 + 361);
        let b = &closed(&closed_eff)[0];
        assert_eq!(b.event_count, 2);
        assert!((b.mean_confidence - 0.7).abs() < 1e-9); // (1.0 + 0.4) / 2
    }

    #[test]
    fn weak_hint_drops_a_running_block_but_never_switches_to_it() {
        // waid is active via autonomous transcript evidence → it's *running* (no prompt
        // ever marked you present). A terminal sitting in another repo fires `cd`s: per §7
        // a running block yields, so the first stray hint *drops* waid — but the stray
        // repo still never becomes a row or the focus (a bare cwd is not focus, §1/§5.3).
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid"));
        let drop = s.ingest(&corrob_ev(30, "whoami"));
        assert_eq!(closed(&drop).len(), 1);
        assert_eq!(closed(&drop)[0].project, "waid");
        assert_eq!(active(&s), None);
        // Further stray hints do nothing — nothing active to drop, whoami still no row.
        for t in (60..400).step_by(30) {
            let eff = s.ingest(&corrob_ev(t, "whoami"));
            assert!(closed(&eff).is_empty() && opened(&eff).is_empty());
        }
        assert_eq!(projects(&s), vec!["waid"]); // waid's row lingers; whoami never appears
    }

    #[test]
    fn present_block_is_not_dropped_by_weak_hint() {
        // You prompted waid (present). A terminal `cd` into another repo must NOT drop it —
        // only a *running* block yields to a weak hint; a present one is protected.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&status_ev(0, "waid", WorkKind::Prompt)); // present
        let eff = s.ingest(&corrob_ev(30, "whoami")); // within recency → still present
        assert!(closed(&eff).is_empty());
        assert_eq!(active(&s).as_deref(), Some("waid"));
        assert_eq!(presence_of(&s, "waid"), Some(Presence::Present));
    }

    #[test]
    fn corroborating_evidence_does_not_cancel_a_real_candidate() {
        // A genuine switch is building (transcript evidence on whoami). A terminal `cd`
        // back in waid must not veto it — primary signal stays in charge. (waid is running
        // here — opened on autonomous `Active` — so its own corroboration just reinforces.)
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

    /// A focus-evidence event with an explicit per-instance `source` id (and optional
    /// label) — the §9 multi-source path.
    fn ev_src(secs: i64, project: &str, source: &str, label: Option<&str>) -> WorkEvent {
        WorkEvent {
            ts: iso(secs),
            surface: Surface::ClaudeCode,
            project: Some(project.to_string()),
            source: Some(source.to_string()),
            source_label: label.map(str::to_string),
            kind: WorkKind::Active,
            confidence: 1.0,
            detail: None,
            work_id: None,
            work_label: None,
            wait: None,
        }
    }

    #[test]
    fn two_sessions_on_one_project_are_two_numbered_sources() {
        // Two concurrent Claude Code sessions in the same repo → one project row with two
        // source rows, numbered by start order (§9: "three sessions read as three rows").
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev_src(0, "whence", "sess-a", None));
        s.ingest(&ev_src(10, "whence", "sess-b", None));
        // Still a single project in the roster...
        assert_eq!(projects(&s), vec!["whence"]);
        // ...but two distinct, numbered source rows under it.
        assert_eq!(source_labels(&s, "whence"), vec!["claude code 1", "claude code 2"]);
        // A repeat on the first session stays one row (same source key), no third row.
        s.ingest(&ev_src(20, "whence", "sess-a", None));
        assert_eq!(source_labels(&s, "whence"), vec!["claude code 1", "claude code 2"]);
    }

    #[test]
    fn distinct_surfaces_keep_their_own_labels_unnumbered() {
        // A Claude Code session and a browser chat on the same project: different base
        // labels → no numbering, each shown as itself (§9 "claude code", "chatgpt web").
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev_src(0, "whence", "sess-a", None));
        let mut browser = ev_src(10, "whence", "https://chatgpt.com/c/1", Some("chatgpt web"));
        browser.surface = Surface::Browser;
        s.ingest(&browser);
        assert_eq!(source_labels(&s, "whence"), vec!["claude code", "chatgpt web"]);
    }

    #[test]
    fn snapshot_surfaces_the_per_instance_source_id() {
        // The id half of the source key is surfaced on the snapshot so the widget can
        // target a click (raise the exact browser tab). A sourceless surface stays `None`.
        let mut s = Segmenter::new(SegmentConfig::default());
        let mut browser = ev_src(0, "whence", "https://claude.ai/project/p1", Some("claude web"));
        browser.surface = Surface::Browser;
        s.ingest(&browser);
        s.ingest(&ev(10, "waid")); // sourceless Claude Code activity → no id to surface
        let snap = s.snapshot();
        let whence = snap.projects.iter().find(|x| x.project == "whence").unwrap();
        assert_eq!(whence.sources[0].source.as_deref(), Some("https://claude.ai/project/p1"));
        let waid = snap.projects.iter().find(|x| x.project == "waid").unwrap();
        assert_eq!(waid.sources[0].source, None);
    }

    #[test]
    fn select_switches_focus_immediately_and_marks_present() {
        // Clicking a source row (a `Select`) is a you-acted override: it switches the
        // active project with no debounce and marks the new block present, exactly like a
        // prompt — even straight off an autonomous block on another project.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev(0, "waid")); // autonomous activity opens a waid block
        s.ingest(&ev(30, "waid"));
        let mut select = ev_src(40, "whence", "https://claude.ai/project/p1", Some("claude web"));
        select.surface = Surface::Browser;
        select.kind = WorkKind::Select;
        let switch = s.ingest(&select);
        // Old waid block closed at its last activity; new whence block opened at t=40.
        let blocks = closed(&switch);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].project, "waid");
        assert_eq!(opened(&switch), vec!["whence"]);
        assert_eq!(active(&s).as_deref(), Some("whence"));
        assert_eq!(presence_of(&s, "whence"), Some(Presence::Present));
    }

    #[test]
    fn project_status_rolls_up_by_attention_priority() {
        // Two sources on one project: one awaiting you, one actively working. The project
        // row rolls up to the highest-priority status (ACTIVE > AWAITING > IDLE, §9).
        let mut s = Segmenter::new(SegmentConfig::default());
        let mut a = ev_src(0, "whence", "sess-a", None);
        a.kind = WorkKind::AwaitingInput; // session A awaiting you
        s.ingest(&a);
        assert_eq!(status_of(&s, "whence"), Some(Status::AwaitingInput));
        s.ingest(&ev_src(10, "whence", "sess-b", None)); // session B active
        assert_eq!(status_of(&s, "whence"), Some(Status::Active));
    }

    #[test]
    fn ending_one_session_keeps_the_project_alive_for_the_other() {
        // Two sessions on whence; one ends. The project (and its block) survives on the
        // remaining session — SessionEnd drops a *source*, not the whole project.
        let mut s = Segmenter::new(SegmentConfig::default());
        s.ingest(&ev_src(0, "whence", "sess-a", None)); // opens the block
        s.ingest(&ev_src(10, "whence", "sess-b", None));
        let mut end_a = ev_src(20, "whence", "sess-a", None);
        end_a.kind = WorkKind::SessionEnd;
        let eff = s.ingest(&end_a);
        // No block closed — whence is still live via sess-b.
        assert!(closed(&eff).is_empty());
        assert_eq!(active(&s).as_deref(), Some("whence"));
        assert_eq!(source_labels(&s, "whence"), vec!["claude code"]); // only sess-b left
        // Ending the last session closes the block and drops the project.
        let mut end_b = ev_src(30, "whence", "sess-b", None);
        end_b.kind = WorkKind::SessionEnd;
        let eff = s.ingest(&end_b);
        assert_eq!(closed(&eff).len(), 1);
        assert!(projects(&s).is_empty());
        assert_eq!(active(&s), None);
    }
}
