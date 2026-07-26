# Plan: Settings as a separate popup window

*Status: implemented (PR #15, 2026-07-26).*

## Context

The settings UI currently renders inline inside the always-on-top widget: clicking the gear grows the 300px-wide widget by 328px (`SECTION_H.settings` in `src/routes/+page.svelte`) and squeezes the 768-line `SettingsPanel.svelte` into it — too long. Convert settings to open in its own decorated popup window when the gear is clicked; the widget stays compact. Clicking the gear while the popup is open focuses it (no duplicate); the native titlebar closes it.

Builds on the working-tree dark-mode feature (`darkMode` setting, `appearance.dark`) — do not disturb it.

## Design decisions

- **Rust `open_settings` command** (not JS `new WebviewWindow`): focus-if-exists-else-create is atomic in the backend, no new JS permission surface, matches this codebase (backend owns window lifecycle — `tray.rs`, `set_settings`).
- **Window:** label `"settings"`, route `/settings`, decorated, title "Whence Settings", 400×560 logical, min 340×420, resizable, opaque, in taskbar, NOT alwaysOnTop, `.center()`. `tauri-plugin-window-state` will remember geometry on later opens — leave it registered as-is.
- **Cross-window propagation:** the `appearance` store is per-JS-realm and won't cross windows. Add a `whence://settings` emit at the end of `set_settings`; the widget listens and refreshes `appearance` on Save. Cross-window *live* preview is dropped: dark mode still previews live inside the settings window (its own realm); the widget updates on Save. This avoids a second event channel and revert-on-close-without-save complexity across realms.
- **`SettingsPanel.svelte` needs zero changes.** Its live-preview `$effect`/`onDestroy` revert now write the settings window's own realm store.

## Changes

### 1. `src-tauri/src/commands.rs`
- Extend imports: `use tauri::{Emitter, Manager, State};`.
- `pub const SETTINGS_EVENT: &str = "whence://settings";` near the top (mirrors `orchestrator::FOCUS_EVENT`).
- In `set_settings`, just before `Ok(settings)` (~line 220): `let _ = app.emit(SETTINGS_EVENT, &settings);` — broadcast so the widget realm picks up appearance changes on Save.
- New command:

```rust
/// Open the settings popup, or focus it if already open. Settings live in their
/// own decorated window (label "settings", route /settings) so the widget stays
/// compact.
#[tauri::command]
pub fn open_settings(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(&app, "settings", tauri::WebviewUrl::App("settings".into()))
        .title("Whence Settings")
        .inner_size(400.0, 560.0)
        .min_inner_size(340.0, 420.0)
        .center()
        .build()
        .map_err(|e| format!("could not open settings window: {e}"))?;
    Ok(())
}
```

### 2. `src-tauri/src/lib.rs`
Register `commands::open_settings` in the `invoke_handler` list.

### 3. `src-tauri/capabilities/default.json`
`"windows": ["main"]` → `"windows": ["main", "settings"]`; update the description. **Without this every invoke/listen in the new window silently fails** (panel stuck on "Loading settings…").

### 4. `src/lib/tauri.ts`
- `export const SETTINGS_EVENT = "whence://settings";`
- `openSettings(): Promise<void>` → `invoke("open_settings")`.
- `onSettingsChanged(cb: (s: Settings) => void): Promise<UnlistenFn>` → `listen<Settings>(SETTINGS_EVENT, …)` (mirror `onNeuroskillStatus`).

### 5. `src/lib/stores/appearance.svelte.ts`
Add a start/stop pair mirroring `neuroskill.svelte.ts`:
- `startAppearance()`: `await loadAppearance()` then `unlisten = await onSettingsChanged((s) => { appearance.opacity = clampOpacity(s.widgetOpacity); appearance.dark = s.darkMode; })`.
- `stopAppearance()`: unlisten + null.
- Keep `loadAppearance` exported (settings route uses it alone). Update the header comment: the panel no longer live-drives the widget; Save events do.

### 6. `src/routes/+page.svelte` (widget cleanup)
- `type View = "compact" | "timeline"`; drop `settings: 328` from `SECTION_H`; remove the `SettingsPanel` import and the `{:else if view === "settings"}` branch (lines 234–236).
- Settings button (~174–181): static `color: var(--fg3)`, `onclick={() => openSettings().catch(() => {})}`; import `openSettings` from `$lib/tauri`.
- Replace the `loadAppearance()` effect (~117–119) with the start/stop pattern: `$effect(() => { startAppearance(); return () => stopAppearance(); });`. Keep the dark-class effect and `.panel` opacity binding as-is.

### 7. New route: `src/routes/settings/+page.svelte`
Hosts `<SettingsPanel />`. Its own realm needs: hydrate appearance (`loadAppearance()`), its own dark-class effect on `<html>`, and `startNeuroskill()/stopNeuroskill()` so the panel's connection dot is live. `app.css` paints `html, body` transparent for the borderless widget, so wrap in an opaque themed container:

```svelte
<script lang="ts">
  import SettingsPanel from "$lib/components/SettingsPanel.svelte";
  import { appearance, loadAppearance } from "$lib/stores/appearance.svelte";
  import { startNeuroskill, stopNeuroskill } from "$lib/stores/neuroskill.svelte";

  $effect(() => { loadAppearance(); });
  $effect(() => {
    document.documentElement.classList.toggle("dark", appearance.dark);
  });
  $effect(() => {
    startNeuroskill();
    return () => stopNeuroskill();
  });
</script>

<!-- select-text overrides the body's inherited user-select:none — the panel
     shows copyable values (receiver token, paths, terminal snippet). -->
<div class="settings-window select-text">
  <SettingsPanel />
</div>

<style>
  .settings-window {
    background: var(--bg);
    height: 100vh;
    display: flex;
    flex-direction: column;
  }
</style>
```

The panel's opacity live-preview write becomes a no-op in this window (nothing binds it) — opacity applies to the widget on Save via the emit, which is the intended behavior.

### No changes
`tauri.conf.json` (window is builder-created), CSP, `+layout.svelte`, `SettingsPanel.svelte`, tray, single-instance, window-state.

## Sequencing
Rust first (1–3), then `tauri.ts` (4), store (5), widget cleanup (6), new route (7). Do the capability edit together with the command — it's the classic silent-failure step.

## Verification
1. `pnpm check` — catches the `View` narrowing and removed import.
2. `cd src-tauri && cargo check` (existing tests unaffected; `cargo test` for safety).
3. Manual via `pnpm tauri:wsl`:
   - Gear click → decorated "Whence Settings" window opens centered, opaque; widget height unchanged.
   - Gear click again → focuses the existing window, no duplicate.
   - Toggle dark mode in the popup → popup restyles live, widget unchanged; Save → widget flips (the `whence://settings` emit). Same for opacity (widget dims only after Save).
   - Close popup without saving → widget unchanged; reopen → persisted values shown; geometry remembered (window-state).
   - NeuroSkill dot live in the panel; always-on-top / always-present still apply to the widget on Save.
4. Prod-path smoke: `pnpm tauri build` (or `--no-bundle`) — the settings window must render the `/settings` route via the adapter-static `fallback: "index.html"` SPA fallback, not a blank page.
