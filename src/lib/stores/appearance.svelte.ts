// Widget appearance, as a Svelte 5 rune: the whole-widget opacity (a constant,
// user-set value — see Settings.widgetOpacity) and the dark-mode flag (see
// Settings.darkMode). The widget binds the `.panel` to `appearance.opacity` and
// mirrors `appearance.dark` onto the `dark` class on <html>; +page hydrates both
// once on mount, and the Settings panel writes through them for live preview.
import { getSettings } from "$lib/tauri";

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
