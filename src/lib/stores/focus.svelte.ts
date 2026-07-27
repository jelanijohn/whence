// Live focus state, as Svelte 5 runes. The widget binds to `focus.snapshot`;
// the backend pushes updates over the `whence://focus` event and we hydrate once
// on mount via get_focus_state.
import type { FocusSnapshot } from "$lib/types";
import { getFocusState, onFocus } from "$lib/tauri";
import type { UnlistenFn } from "@tauri-apps/api/event";

export const focus = $state<{ snapshot: FocusSnapshot }>({
  snapshot: { projects: [] },
});

let unlisten: UnlistenFn | null = null;

export async function startFocus(): Promise<void> {
  // Hydrate immediately, then keep current via the event stream.
  try {
    focus.snapshot = await getFocusState();
  } catch {
    // Backend not up yet (e.g. `vite dev` without Tauri) — stay on the default.
  }
  try {
    unlisten = await onFocus((snap) => {
      focus.snapshot = snap;
    });
  } catch {
    // No event API either — the snapshot just stays at its hydrated value.
  }
}

export function stopFocus(): void {
  unlisten?.();
  unlisten = null;
}
