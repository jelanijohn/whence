// The single place that names backend commands and events. Everything else in
// the frontend imports from here so the Rust contract has one front door.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  FocusSnapshot,
  FocusBlock,
  Settings,
  NeuroskillStatus,
  ReceiverAuth,
} from "./types";

export const FOCUS_EVENT = "whence://focus";
export const NEUROSKILL_EVENT = "whence://neuroskill";
export const SETTINGS_EVENT = "whence://settings";

// Mock mode (VITE_WHENCE_MOCK=1): serve canned data for headless screenshots
// (scripts/screenshots.mjs). The env check is statically replaced by Vite, so
// real builds drop the branch — and with it the dynamically imported mock
// chunk — entirely.
const MOCK = import.meta.env.VITE_WHENCE_MOCK === "1";

function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (MOCK) return import("./tauri.mock").then((m) => m.mockInvoke<T>(cmd, args));
  return invoke<T>(cmd, args);
}

function subscribe<T>(
  event: string,
  cb: (payload: T) => void,
): Promise<UnlistenFn> {
  if (MOCK) return import("./tauri.mock").then((m) => m.mockListen<T>(event, cb));
  return listen<T>(event, (e) => cb(e.payload));
}

/** Current focus snapshot — every live session (each with status + row-timer start). */
export function getFocusState(): Promise<FocusSnapshot> {
  return call("get_focus_state");
}

/** Raise the browser tab for `source` and pull `project` into focus. Browser source
 *  rows only — `source` is the row's normalized conversation URL. */
export function focusSource(project: string, source: string): Promise<void> {
  return call("focus_source", { project, source });
}

/** Today's closed focus blocks, oldest first — drives the expanded timeline. */
export function getTodayBlocks(): Promise<FocusBlock[]> {
  return call("get_today_blocks");
}

/** Mean EEG focus (0..100) over the recent window, or null when the optional
 *  read-back isn't available (feature off, no daemon store, no recent epochs).
 *  Drives the widget's intensity meter. */
export function getFocusIntensity(): Promise<number | null> {
  return call("get_focus_intensity");
}

export function getSettings(): Promise<Settings> {
  return call("get_settings");
}

/** Absolute path to the browser-adapter mapping file (browser_mapping.toml) — shown
 *  in settings as a hand-editable deep-link. */
export function getBrowserMappingPath(): Promise<string> {
  return call("get_browser_mapping_path");
}

/** Persist settings. Toggling `autostart` registers/unregisters the launch agent. */
export function setSettings(settings: Settings): Promise<Settings> {
  return call("set_settings", { settings });
}

/** Open the settings popup window, or focus it if already open. */
export function openSettings(): Promise<void> {
  return call("open_settings");
}

/** Install (opt-in) the Claude Code `http` hooks that feed live `awaiting_input`
 *  status. Merge-preserving write to ~/.claude/settings.json. */
export function installClaudeHooks(): Promise<void> {
  return call("install_claude_hooks");
}

/** Remove Whence's Claude Code hooks, leaving other config intact. */
export function uninstallClaudeHooks(): Promise<void> {
  return call("uninstall_claude_hooks");
}

/** Receiver-auth state: the bearer token gating the loopback receivers, its file
 *  path, and the rejected-request counter. Drives the Settings auth section. */
export function getReceiverAuth(): Promise<ReceiverAuth> {
  return call("get_receiver_auth");
}

/** Mint + persist a new receiver token and apply it live; installed Claude hooks
 *  are rewritten to the new URL. Terminal snippet / extension need a re-paste. */
export function rotateReceiverToken(): Promise<ReceiverAuth> {
  return call("rotate_receiver_token");
}

/** Subscribe to live focus updates. Returns an unlisten fn. */
export function onFocus(cb: (snap: FocusSnapshot) => void): Promise<UnlistenFn> {
  return subscribe(FOCUS_EVENT, cb);
}

/** Current NeuroSkill connection status — for first paint of the header indicator. */
export function getNeuroskillStatus(): Promise<NeuroskillStatus> {
  return call("get_neuroskill_status");
}

/** Subscribe to NeuroSkill connection-status changes. Returns an unlisten fn. */
export function onNeuroskillStatus(
  cb: (status: NeuroskillStatus) => void,
): Promise<UnlistenFn> {
  return subscribe(NEUROSKILL_EVENT, cb);
}

/** Subscribe to settings saves (broadcast from `set_settings`) — how the widget
 *  realm picks up appearance changes made in the settings window. */
export function onSettingsChanged(
  cb: (settings: Settings) => void,
): Promise<UnlistenFn> {
  return subscribe(SETTINGS_EVENT, cb);
}
