// Widget appearance, as a Svelte 5 rune: the whole-widget opacity (a constant,
// user-set value — see Settings.widgetOpacity) and the dark-mode flag (see
// Settings.darkMode). The widget binds the `.panel` to `appearance.opacity` and
// mirrors `appearance.dark` onto the `dark` class on <html>. Settings live in
// their own window (own JS realm), so the panel no longer live-drives the
// widget: the widget hydrates once via `startAppearance` and then tracks the
// `whence://settings` broadcast fired on every Save. The settings window still
// gets live preview — the panel writes through this store in its *own* realm.
import { getSettings, onSettingsChanged } from "$lib/tauri";
import type { UnlistenFn } from "@tauri-apps/api/event";

// Floor matches the UI slider min and the Rust doc — never let the widget go
// unreadable / effectively un-clickable.
export const MIN_OPACITY = 0.3;

/** Clamp any value into the legal `[MIN_OPACITY, 1]` opacity range. */
export function clampOpacity(n: number): number {
  return Math.min(1, Math.max(MIN_OPACITY, Number(n) || 1));
}

export const appearance = $state<{ opacity: number; dark: boolean }>({
  opacity: 1,
  dark: false,
});

/** Hydrate the live appearance from persisted settings. Best-effort: a missing
 *  backend (e.g. `vite dev` without Tauri) just leaves the defaults. */
export async function loadAppearance(): Promise<void> {
  try {
    const s = await getSettings();
    appearance.opacity = clampOpacity(s.widgetOpacity);
    appearance.dark = s.darkMode;
  } catch {
    // Backend not up — stay on the defaults.
  }
}

let unlisten: UnlistenFn | null = null;

/** Hydrate once, then track settings saves — the widget's cross-window feed. */
export async function startAppearance(): Promise<void> {
  await loadAppearance();
  unlisten = await onSettingsChanged((s) => {
    appearance.opacity = clampOpacity(s.widgetOpacity);
    appearance.dark = s.darkMode;
  });
}

export function stopAppearance(): void {
  unlisten?.();
  unlisten = null;
}
