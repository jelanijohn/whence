// The single place that names backend commands and events. Everything else in
// the frontend imports from here so the Rust contract has one front door.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { FocusSnapshot, FocusBlock, Settings } from "./types";

export const FOCUS_EVENT = "whence://focus";

/** Current focus snapshot (project + status + open-block start). */
export function getFocusState(): Promise<FocusSnapshot> {
  return invoke("get_focus_state");
}

/** Today's closed focus blocks, oldest first — drives the expanded timeline. */
export function getTodayBlocks(): Promise<FocusBlock[]> {
  return invoke("get_today_blocks");
}

export function getSettings(): Promise<Settings> {
  return invoke("get_settings");
}

/** Persist settings. Toggling `autostart` registers/unregisters the launch agent. */
export function setSettings(settings: Settings): Promise<Settings> {
  return invoke("set_settings", { settings });
}

/** Subscribe to live focus updates. Returns an unlisten fn. */
export function onFocus(cb: (snap: FocusSnapshot) => void): Promise<UnlistenFn> {
  return listen<FocusSnapshot>(FOCUS_EVENT, (e) => cb(e.payload));
}
