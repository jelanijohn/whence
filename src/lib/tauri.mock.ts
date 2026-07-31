// Canned backend for headless screenshots (`VITE_WHENCE_MOCK=1`). Serves a
// believable posed scenario through the same `invoke`/`listen` shapes as the
// real backend, so `tauri.ts` can branch at its two choke points and nothing
// else in the frontend knows the difference. Pure data + two functions — no
// module side effects, so real builds tree-shake this file away entirely
// (verified by grepping built output for WHENCE_MOCK_MARKER).
import type { UnlistenFn } from "@tauri-apps/api/event";
import type {
  FocusSnapshot,
  FocusBlock,
  Settings,
  NeuroskillStatus,
  ReceiverAuth,
} from "./types";

/** Distinctive string to grep built output for, proving the mock never ships. */
export const WHENCE_MOCK_MARKER = "whence-mock-data";

// All row timers count up from `statusSince`, so offsets are taken from module
// load — the capture happens seconds later and the timers read as intended
// (whence ≈ 18:32 deep, glue-mac ≈ 6:12 waiting, neuroskill ≈ 45:05 idle).
const NOW = Math.floor(Date.now() / 1000);

/** Today at a fixed local wall-clock time — timeline blocks print a believable workday. */
function todayAt(hours: number, minutes: number): number {
  const d = new Date();
  d.setHours(hours, minutes, 0, 0);
  return Math.floor(d.getTime() / 1000);
}

const focusSnapshot: FocusSnapshot = {
  projects: [
    {
      project: "whence",
      status: "active",
      statusSince: NOW - 1112,
      active: true,
      presence: "present",
      sources: [
        {
          surface: "claude-code",
          source: null,
          label: "claude code",
          status: "active",
          statusSince: NOW - 1112,
        },
        {
          surface: "terminal",
          source: null,
          label: "terminal",
          status: "active",
          statusSince: NOW - 754,
        },
      ],
    },
    {
      project: "glue-mac",
      status: "awaiting_input",
      statusSince: NOW - 372,
      active: false,
      presence: null,
      sources: [
        {
          surface: "claude-code",
          source: null,
          label: "claude code",
          status: "awaiting_input",
          statusSince: NOW - 372,
        },
      ],
    },
    {
      project: "neuroskill",
      status: "idle",
      statusSince: NOW - 2705,
      active: false,
      presence: null,
      sources: [
        {
          surface: "browser",
          source: "https://claude.ai/chat/abc123",
          label: "claude web",
          status: "idle",
          statusSince: NOW - 2705,
        },
      ],
    },
  ],
  context: { text: "main · fix HEAD parser", source: "git", observedAt: NOW - 40 },
};

const todayBlocks: FocusBlock[] = [
  {
    project: "whence",
    start: todayAt(9, 4),
    end: todayAt(10, 26),
    eventCount: 148,
    meanConfidence: 0.92,
    context: { text: "main · scaffold settings window", source: "git" },
  },
  {
    project: "glue-mac",
    start: todayAt(10, 26),
    end: todayAt(11, 12),
    eventCount: 61,
    meanConfidence: 0.88,
    context: { text: "main · retry temporal activity", source: "git" },
  },
  {
    project: "whence",
    start: todayAt(11, 12),
    end: todayAt(11, 31),
    eventCount: 23,
    meanConfidence: 0.9,
  },
  {
    project: "neuroskill",
    start: todayAt(12, 58),
    end: todayAt(14, 3),
    eventCount: 87,
    meanConfidence: 0.84,
    context: { text: "docs · auth flow notes", source: "git" },
  },
  {
    project: "whence",
    start: todayAt(14, 20),
    end: todayAt(15, 47),
    eventCount: 132,
    meanConfidence: 0.95,
    context: { text: "main · fix HEAD parser", source: "git" },
  },
];

// Mirrors `impl Default for Settings` in src-tauri/src/settings.rs, camelCase
// per the serde rename — so the settings window screenshots show real defaults.
const settings: Settings = {
  autostart: false,
  neuroskillEnabled: true,
  neuroskillEndpoint: null,
  neuroskillTokenPath: null,
  neuroskillDataDir: null,
  // Defaults to false like the Rust side; the mock still answers
  // get_focus_intensity with a value so roster screenshots show the meter.
  eegReadbackEnabled: false,
  claudeDir: null,
  projectAliases: {},
  hookListenAddrOverride: null,
  ollamaEnabled: false,
  ollamaEndpoint: null,
  terminalEnabled: false,
  terminalListenAddrOverride: null,
  browserEnabled: false,
  browserListenAddrOverride: null,
  contextStrings: true,
  contextTtlSeconds: 60,
  contextHookPrompts: false,
  contextBrowserTitles: false,
  switchMinSeconds: 90,
  idleTimeoutSeconds: 360,
  corroboratorConfidenceCutoff: 0.6,
  attentionRecencySeconds: 120,
  widgetOpacity: 1.0,
  darkMode: false,
  alwaysOnTop: true,
  alwaysPresent: false,
};

// Neutral placeholder username — these paths render in the settings screenshots,
// so no real account name belongs here.
const MOCK_DATA_DIR = "/home/user/.local/share/com.jelanijohn.whence";

function receiverAuth(token: string): ReceiverAuth {
  return {
    token,
    tokenPath: `${MOCK_DATA_DIR}/receiver.token`,
    denials: 0,
  };
}

/** Canned command results, keyed exactly like the Rust commands. */
function respond(cmd: string, args?: Record<string, unknown>): unknown {
  switch (cmd) {
    case "get_focus_state":
      return focusSnapshot;
    case "get_today_blocks":
      return todayBlocks;
    case "get_focus_intensity":
      return 72;
    case "get_neuroskill_status":
      return "connected" satisfies NeuroskillStatus;
    case "get_settings":
      return settings;
    case "set_settings":
      // Echo back like the real command, so Save in the settings window works.
      return args?.settings ?? settings;
    case "get_receiver_auth":
      return receiverAuth("9f3a1c8e2b7d4f60a5c3e1d9b8f2a604");
    case "rotate_receiver_token":
      return receiverAuth("4d8b2f6a1e9c7305b6d4f2a0c8e13b57");
    case "get_browser_mapping_path":
      return `${MOCK_DATA_DIR}/browser_mapping.toml`;
    case "focus_source":
    case "open_settings":
    case "install_claude_hooks":
    case "uninstall_claude_hooks":
      return undefined;
    default:
      // A command the mock doesn't know = a contract drift; fail loudly rather
      // than screenshot a silently-undefined state.
      throw new Error(`${WHENCE_MOCK_MARKER}: unmocked command "${cmd}"`);
  }
}

export function mockInvoke<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return Promise.resolve(respond(cmd, args) as T);
  } catch (e) {
    return Promise.reject(e);
  }
}

export function mockListen<T>(
  _event: string,
  _cb: (payload: T) => void,
): Promise<UnlistenFn> {
  // Static shots need no live pushes — subscriptions succeed and never fire.
  return Promise.resolve(() => {});
}
