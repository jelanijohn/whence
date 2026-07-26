// Live NeuroSkill connection status, as a Svelte 5 rune. Mirrors focus.svelte.ts:
// hydrate once on mount via get_neuroskill_status, then track the backend's
// `whence://neuroskill` pushes (emitted only when the status changes).
import type { NeuroskillStatus } from "$lib/types";
import { getNeuroskillStatus, onNeuroskillStatus } from "$lib/tauri";
import type { UnlistenFn } from "@tauri-apps/api/event";

export const neuroskill = $state<{ status: NeuroskillStatus }>({
  status: "unknown",
});

let unlisten: UnlistenFn | null = null;

export async function startNeuroskill(): Promise<void> {
  try {
    neuroskill.status = await getNeuroskillStatus();
  } catch {
    // Backend not up yet (e.g. `vite dev` without Tauri) — stay on "unknown".
  }
  try {
    unlisten = await onNeuroskillStatus((status) => {
      neuroskill.status = status;
    });
  } catch {
    // No event API either — status just stays at its hydrated value.
  }
}

export function stopNeuroskill(): void {
  unlisten?.();
  unlisten = null;
}
