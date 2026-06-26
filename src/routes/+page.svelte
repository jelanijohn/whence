<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { focus, startFocus, stopFocus } from "$lib/stores/focus.svelte";
  import {
    neuroskill,
    startNeuroskill,
    stopNeuroskill,
  } from "$lib/stores/neuroskill.svelte";
  import { getTodayBlocks, getFocusIntensity } from "$lib/tauri";
  import type { FocusBlock } from "$lib/types";
  import BrandMark from "$lib/components/BrandMark.svelte";
  import SessionRow from "$lib/components/SessionRow.svelte";
  import IntensityMeter from "$lib/components/IntensityMeter.svelte";
  import NeuroskillStatusDot from "$lib/components/NeuroskillStatusDot.svelte";
  import BlockTimeline from "$lib/components/BlockTimeline.svelte";
  import SettingsPanel from "$lib/components/SettingsPanel.svelte";

  type View = "compact" | "timeline" | "settings";

  // The compact area is a title bar + N session rows, so its height is dynamic.
  // Expanded views add a fixed section below it.
  const TITLE_H = 44;
  const ROW_H = 30;
  const LIST_PAD = 12;
  const SECTION_H: Record<Exclude<View, "compact">, number> = {
    timeline: 168,
    settings: 328,
  };
  const WIDTH = 300;

  let view = $state<View>("compact");
  let blocks = $state<FocusBlock[]>([]);
  // Optional EEG intensity (null = read-back unavailable; the meter hides itself).
  // Polled rather than pushed — it's a slow-moving read-back, not a focus event.
  let intensity = $state<number | null>(null);

  // Active session first, then alphabetical — so the focused project leads the list.
  const sessions = $derived(
    [...focus.snapshot.sessions].sort((a, b) =>
      a.active !== b.active
        ? a.active
          ? -1
          : 1
        : a.project.localeCompare(b.project),
    ),
  );

  function windowHeight(v: View, rowCount: number): number {
    const compact = TITLE_H + Math.max(1, rowCount) * ROW_H + LIST_PAD;
    return v === "compact" ? compact : compact + SECTION_H[v];
  }

  async function resize(height: number) {
    try {
      const { LogicalSize } = await import("@tauri-apps/api/dpi");
      await getCurrentWindow().setSize(new LogicalSize(WIDTH, height));
    } catch {
      // No Tauri window (e.g. `vite dev` without the backend) — nothing to size.
    }
  }

  $effect(() => {
    startFocus();
    return () => stopFocus();
  });

  $effect(() => {
    startNeuroskill();
    return () => stopNeuroskill();
  });

  // Keep the window sized to the current view + live session count.
  $effect(() => {
    resize(windowHeight(view, sessions.length));
  });

  $effect(() => {
    let alive = true;
    const poll = async () => {
      const v = await getFocusIntensity().catch(() => null);
      if (alive) intensity = v;
    };
    poll();
    const id = setInterval(poll, 15000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  });

  // Header buttons toggle their view; clicking the active one returns to compact.
  async function setView(target: Exclude<View, "compact">) {
    view = view === target ? "compact" : target;
    if (view === "timeline") blocks = await getTodayBlocks().catch(() => []);
  }
</script>

<div class="panel select-none">
  <!-- App title bar. The whole bar moves the window (data-tauri-drag-region). -->
  <header
    data-tauri-drag-region
    class="flex items-center justify-between gap-2 px-3 pt-3 pb-2"
  >
    <span class="inline-flex items-center gap-2">
      <BrandMark size={18} />
      <span class="font-semibold" style="color: var(--fg); font-size: 15px;"
        >Whence</span
      >
    </span>
    <div class="flex shrink-0 items-center gap-2">
      <NeuroskillStatusDot status={neuroskill.status} />
      <IntensityMeter value={intensity} />
      <button
        class="msym"
        style="color: {view === 'settings' ? 'var(--accent)' : 'var(--fg3)'}; font-size: 18px; cursor: pointer;"
        title="Settings"
        onclick={() => setView("settings")}
      >
        settings
      </button>
      <button
        class="msym"
        style="color: {view === 'timeline' ? 'var(--accent)' : 'var(--fg3)'}; font-size: 18px; cursor: pointer;"
        title={view === "timeline" ? "Collapse" : "Today's blocks"}
        onclick={() => setView("timeline")}
      >
        {view === "timeline" ? "expand_less" : "expand_more"}
      </button>
    </div>
  </header>

  <!-- Live sessions: one row per project, or a quiet idle row when nothing's live. -->
  <div class="pb-2">
    {#if sessions.length === 0}
      <div
        class="flex items-center gap-2 px-3 py-1"
        style="opacity: 0.55; color: var(--fg3); font-size: 14px;"
      >
        <span class="inline-block shrink-0" style="width: 16px;"></span>
        idle
      </div>
    {:else}
      {#each sessions as session (session.project)}
        <SessionRow {session} />
      {/each}
    {/if}
  </div>

  {#if view !== "compact"}
    <div
      class="flex min-h-0 flex-1 flex-col overflow-hidden"
      style="border-top: 1px solid var(--border-soft);"
    >
      {#if view === "timeline"}
        <div class="overflow-y-auto pt-2">
          <BlockTimeline {blocks} />
        </div>
      {:else if view === "settings"}
        <SettingsPanel />
      {/if}
    </div>
  {/if}
</div>
