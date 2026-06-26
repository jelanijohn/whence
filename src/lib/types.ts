// Mirrors the Rust models in src-tauri/src/adapters/mod.rs and engine/segment.rs.
// Keep these in sync — the serde representations must match field-for-field.

export type Surface = "claude-code" | "ollama" | "terminal" | "claude-desktop";

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

// A closed focus block, as persisted to the JSONL timeline.
export interface FocusBlock {
  project: string;
  start: number; // unix seconds
  end: number; // unix seconds
  eventCount: number;
  meanConfidence: number;
}

// The live snapshot the widget renders, pushed on the `whence://focus` event
// and returned by the get_focus_state command.
export interface FocusSnapshot {
  project: string | null; // current focus, null = unattributed/idle
  status: Status;
  blockStart: number | null; // unix seconds; null when no open block
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
  switchMinSeconds: number; // sustained evidence to confirm a switch
  idleTimeoutSeconds: number; // gap that ends a block
}
