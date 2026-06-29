// Widget appearance, as a Svelte 5 rune. Currently just the whole-widget opacity
// (a constant, user-set value — see Settings.widgetOpacity). The widget binds the
// `.panel` to `appearance.opacity`; +page hydrates it once on mount, and the
// Settings panel writes through it for live preview while the slider moves.
import { getSettings } from "$lib/tauri";

// Floor matches the UI slider min and the Rust doc — never let the widget go
// unreadable / effectively un-clickable.
export const MIN_OPACITY = 0.3;

/** Clamp any value into the legal `[MIN_OPACITY, 1]` opacity range. */
export function clampOpacity(n: number): number {
  return Math.min(1, Math.max(MIN_OPACITY, Number(n) || 1));
}

export const appearance = $state<{ opacity: number }>({ opacity: 1 });

/** Hydrate the live opacity from persisted settings. Best-effort: a missing
 *  backend (e.g. `vite dev` without Tauri) just leaves it fully opaque. */
export async function loadAppearance(): Promise<void> {
  try {
    const s = await getSettings();
    appearance.opacity = clampOpacity(s.widgetOpacity);
  } catch {
    // Backend not up — stay on the default.
  }
}
