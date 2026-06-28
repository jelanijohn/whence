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
  | "session_end";

export interface WorkEvent {
  ts: string; // ISO-8601
  surface: Surface;
  project: string | null; // resolved slug; null = unattributed
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

// One live session row — one per project. The widget renders all of them; only the
// `active` one drives the NeuroSkill label + timeline (attribution stays single).
export interface SessionSnapshot {
  project: string;
  status: Status;
  blockStart: number | null; // unix seconds; the row timer counts from here
  active: boolean; // the single focused project
  presence: Presence | null; // set only on the active row (§7); null otherwise
}

// The live snapshot the widget renders, pushed on the `whence://focus` event and
// returned by the get_focus_state command. Empty `sessions` = idle / nothing live.
export interface FocusSnapshot {
  sessions: SessionSnapshot[];
}

export interface Settings {
  autostart: boolean; // opt-in, default false (never silently)
  neuroskillEnabled: boolean; // write attribution labels into NeuroSkill
  neuroskillEndpoint?: string | null; // override daemon URL; null = default :18444
  neuroskillTokenPath?: string | null; // override token file; null = auto (native → WSL2 host)
  neuroskillDataDir?: string | null; // override data dir (activity.sqlite); null = auto; powers the intensity meter
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
}
