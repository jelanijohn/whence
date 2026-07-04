// Mirrors the Rust models in src-tauri/src/adapters/mod.rs and engine/segment.rs.
// Keep these in sync — the serde representations must match field-for-field.

export type Surface =
  | "claude-code"
  | "ollama"
  | "terminal"
  | "claude-desktop"
  | "browser";

export type WorkKind =
  | "session_start"
  | "prompt"
  | "tool_use"
  | "awaiting_input"
  | "active"
  | "idle"
  | "session_end"
  | "select"; // a manual click on a source row — a you-acted focus override

export interface WorkEvent {
  ts: string; // ISO-8601
  surface: Surface;
  project: string | null; // resolved slug; null = unattributed
  source?: string | null; // stable per-instance source key within (project, surface)
  source_label?: string | null; // display hint (e.g. "chatgpt web"); null = use surface
  kind: WorkKind;
  confidence: number; // 0..1
  detail?: string | null;
}

export type Status = "active" | "awaiting_input" | "idle";

// Present vs running for the active block (§7). present = you acted within the
// attention-recency window (you're here); running = only autonomous activity since
// (Claude's going, you may have stepped away). Only the active row carries one.
export type Presence = "present" | "running";

// NeuroSkill daemon connection health, surfaced by the backend health probe.
// Mirrors `NeuroskillStatus` in src-tauri/src/neuroskill/health.rs (snake_case).
//   disabled     — label writing turned off; we don't probe
//   connected    — daemon reachable + token accepted
//   unauthorized — reachable but auth rejected (token missing/wrong)
//   unreachable  — daemon not reachable (down / wrong endpoint)
//   unknown      — not probed yet
export type NeuroskillStatus =
  | "disabled"
  | "connected"
  | "unauthorized"
  | "unreachable"
  | "unknown";

// A closed focus block, as persisted to the JSONL timeline.
export interface FocusBlock {
  project: string;
  start: number; // unix seconds
  end: number; // unix seconds
  eventCount: number;
  meanConfidence: number;
}

// One live source under a project row — a single Claude Code session, browser
// conversation, or terminal (§9). Revealed when a project row is expanded.
export interface SourceSnapshot {
  surface: Surface;
  source?: string | null; // per-instance source id (browser = normalized URL); null = none
  label: string; // display label; numbered ("terminal 1") when a kind repeats
  status: Status;
  statusSince: number; // unix seconds; the source's time-in-status timer base
}

// One project row in the roster (§9) — one per project with a live/recent source.
// The widget renders all of them; only the `active` one drives the NeuroSkill label +
// timeline (attribution stays single). Its `sources` are the per-session breakdown.
export interface ProjectSnapshot {
  project: string;
  status: Status; // attention-priority roll-up of the sources' statuses
  statusSince: number | null; // unix seconds; the row timer counts from here
  active: boolean; // the single focused project
  presence: Presence | null; // set only on the active row (§7); null otherwise
  sources: SourceSnapshot[];
}

// The live snapshot the widget renders, pushed on the `whence://focus` event and
// returned by the get_focus_state command. Empty `projects` = idle / nothing live.
export interface FocusSnapshot {
  projects: ProjectSnapshot[];
}

export interface Settings {
  autostart: boolean; // opt-in, default false (never silently)
  neuroskillEnabled: boolean; // write attribution labels into NeuroSkill
  neuroskillEndpoint?: string | null; // override daemon URL; null = default :18444
  neuroskillTokenPath?: string | null; // override token file; null = auto (native → WSL2 host)
  neuroskillDataDir?: string | null; // override data dir (activity.sqlite); null = auto; powers the intensity meter
  claudeDir?: string | null; // override the .claude dir (transcripts + hooks install); null = auto (native home → WSL distro walk on Windows)
  projectAliases: Record<string, string>; // transcript dir-name → canonical slug
  hookListenAddrOverride?: string | null; // override hook receiver bind; null = default 127.0.0.1:18450
  ollamaEnabled: boolean; // poll Ollama for inference liveness (low-confidence status only)
  ollamaEndpoint?: string | null; // override Ollama API origin; null = default http://localhost:11434
  terminalEnabled: boolean; // receive shell cwd hints (low-confidence corroborator only)
  terminalListenAddrOverride?: string | null; // override cwd receiver bind; null = default 127.0.0.1:18451
  browserEnabled: boolean; // receive browser LLM sessions from the first-party extension (originating-capable)
  browserListenAddrOverride?: string | null; // override browser receiver bind; null = default 127.0.0.1:18452
  switchMinSeconds: number; // sustained evidence to confirm a switch
  idleTimeoutSeconds: number; // gap that ends a block
  corroboratorConfidenceCutoff: number; // ≥ this = primary signal; below = weak hint (§7)
  attentionRecencySeconds: number; // present-vs-running boundary: act recency window (§7)
  widgetOpacity: number; // whole-widget opacity, 0.3..1 (1 = opaque); constant, frontend-only
  alwaysOnTop: boolean; // float above other windows; default true
  alwaysPresent: boolean; // visible on all workspaces/desktops; macOS + Linux (GNOME) only, default false
}
