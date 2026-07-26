<script lang="ts">
  // The settings popup window (label "settings", opened by `open_settings`).
  // Its own JS realm, so it hydrates appearance and mirrors the dark class
  // itself; NeuroSkill start/stop keeps the panel's connection dot live.
  import SettingsPanel from "$lib/components/SettingsPanel.svelte";
  import { appearance, loadAppearance } from "$lib/stores/appearance.svelte";
  import { startNeuroskill, stopNeuroskill } from "$lib/stores/neuroskill.svelte";

  $effect(() => {
    loadAppearance();
  });
  $effect(() => {
    document.documentElement.classList.toggle("dark", appearance.dark);
  });
  $effect(() => {
    startNeuroskill();
    return () => stopNeuroskill();
  });
</script>

<!-- app.css paints html/body transparent for the borderless widget; this
     decorated window needs an opaque themed backdrop. -->
<div class="settings-window select-none">
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
