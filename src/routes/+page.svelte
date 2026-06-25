<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { focus, startFocus, stopFocus } from "$lib/stores/focus.svelte";
  import { getTodayBlocks } from "$lib/tauri";
  import type { FocusBlock } from "$lib/types";
  import FocusBadge from "$lib/components/FocusBadge.svelte";
  import StatusDot from "$lib/components/StatusDot.svelte";
  import BlockTimer from "$lib/components/BlockTimer.svelte";
  import BlockTimeline from "$lib/components/BlockTimeline.svelte";
  import SettingsPanel from "$lib/components/SettingsPanel.svelte";

  type View = "compact" | "timeline" | "settings";
  const SIZES: Record<View, { width: number; height: number }> = {
    compact: { width: 300, height: 132 },
    timeline: { width: 300, height: 300 },
    settings: { width: 300, height: 460 },
  };

  let view = $state<View>("compact");
  let blocks = $state<FocusBlock[]>([]);

  $effect(() => {
    startFocus();
    return () => stopFocus();
  });

  // Header buttons toggle their view; clicking the active one returns to compact.
  async function setView(target: Exclude<View, "compact">) {
    view = view === target ? "compact" : target;
    if (view === "timeline") blocks = await getTodayBlocks().catch(() => []);
    const { LogicalSize } = await import("@tauri-apps/api/dpi");
    const size = SIZES[view];
    await getCurrentWindow().setSize(new LogicalSize(size.width, size.height));
  }
</script>

<div class="panel select-none">
  <!-- Drag region: the whole header moves the window (data-tauri-drag-region). -->
  <header
    data-tauri-drag-region
    class="flex items-center justify-between gap-2 px-3 pt-3 pb-2"
  >
    <FocusBadge project={focus.snapshot.project} />
    <div class="flex shrink-0 items-center gap-1.5">
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

  <div class="flex items-center justify-between px-3 pb-3">
    <StatusDot status={focus.snapshot.status} />
    <BlockTimer start={focus.snapshot.blockStart} />
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
