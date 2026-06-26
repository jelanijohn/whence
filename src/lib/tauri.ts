// The single place that names backend commands and events. Everything else in
// the frontend imports from here so the Rust contract has one front door.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { FocusSnapshot, FocusBlock, Settings } from "./types";

export const FOCUS_EVENT = "whence://focus";

/** Current focus snapshot — every live session (each with status + row-timer start). */
export function getFocusState(): Promise<FocusSnapshot> {
  return invoke("get_focus_state");
}

/** Today's closed focus blocks, oldest first — drives the expanded timeline. */
export function getTodayBlocks(): Promise<FocusBlock[]> {
  return invoke("get_today_blocks");
}

/** Mean EEG focus (0..100) over the recent window, or null when the optional
 *  read-back isn't available (feature off, no daemon store, no recent epochs).
 *  Drives the widget's intensity meter. */
export function getFocusIntensity(): Promise<number | null> {
  return invoke("get_focus_intensity");
}

export function getSettings(): Promise<Settings> {
  return invoke("get_settings");
}

/** Persist settings. Toggling `autostart` registers/unregisters the launch agent. */
export function setSettings(settings: Settings): Promise<Settings> {
  return invoke("set_settings", { settings });
}

/** Install (opt-in) the Claude Code `http` hooks that feed live `awaiting_input`
 *  status. Merge-preserving write to ~/.claude/settings.json. */
export function installClaudeHooks(): Promise<void> {
  return invoke("install_claude_hooks");
}

/** Remove Whence's Claude Code hooks, leaving other config intact. */
export function uninstallClaudeHooks(): Promise<void> {
  return invoke("uninstall_claude_hooks");
}

/** Subscribe to live focus updates. Returns an unlisten fn. */
export function onFocus(cb: (snap: FocusSnapshot) => void): Promise<UnlistenFn> {
  return listen<FocusSnapshot>(FOCUS_EVENT, (e) => cb(e.payload));
}
