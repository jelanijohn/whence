<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { focus, startFocus, stopFocus } from "$lib/stores/focus.svelte";
  import {
    neuroskill,
    startNeuroskill,
    stopNeuroskill,
  } from "$lib/stores/neuroskill.svelte";
  import { appearance, loadAppearance } from "$lib/stores/appearance.svelte";
  import { getTodayBlocks, getFocusIntensity, focusSource } from "$lib/tauri";
  import type { FocusBlock, SourceSnapshot } from "$lib/types";
  import type { Status } from "$lib/types";
  import BrandMark from "$lib/components/BrandMark.svelte";
  import ProjectRow from "$lib/components/ProjectRow.svelte";
  import IntensityMeter from "$lib/components/IntensityMeter.svelte";
  import NeuroskillStatusDot from "$lib/components/NeuroskillStatusDot.svelte";
  import BlockTimeline from "$lib/components/BlockTimeline.svelte";
  import SettingsPanel from "$lib/components/SettingsPanel.svelte";

  type View = "compact" | "timeline" | "settings";

  // The compact area is a title bar + N project rows (+ any expanded source rows),
  // so its height is dynamic. Expanded views add a fixed section below it.
  const TITLE_H = 44;
  const ROW_H = 30;
  const SOURCE_ROW_H = 24;
  // The focused row's context-string line (docs/context-strings.md §6) — only
  // counted when one is present; no reserved height otherwise.
  const CONTEXT_ROW_H = 16;
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

  // Roster order (§9): the focus project leads, then by attention priority
  // (ACTIVE → AWAITING YOU → IDLE), then alphabetical. A priority sort, never a
  // ranking of how fragmented the day was (principle 3).
  const RANK: Record<Status, number> = { active: 2, awaiting_input: 1, idle: 0 };
  const projects = $derived(
    [...focus.snapshot.projects].sort((a, b) => {
      if (a.active !== b.active) return a.active ? -1 : 1;
      if (RANK[a.status] !== RANK[b.status]) return RANK[b.status] - RANK[a.status];
      return a.project.localeCompare(b.project);
    }),
  );

  // Which project rows are expanded to show their sources. Keyed by slug, so a row
  // that briefly drops and returns keeps its state.
  let expanded = $state<Set<string>>(new Set());
  function toggle(slug: string): void {
    const next = new Set(expanded);
    next.has(slug) ? next.delete(slug) : next.add(slug);
    expanded = next;
  }
  const expandedSourceCount = $derived(
    projects.reduce(
      (acc, p) => acc + (expanded.has(p.project) ? p.sources.length : 0),
      0,
    ),
  );

  // Click a (browser) source row: raise its tab and pull the project into focus. Only
  // browser rows surface a `source` id and an `onActivate`, so this never fires for the
  // unraisable surfaces. Best-effort — a failed invoke (backend down) is swallowed.
  async function activateSource(project: string, source: SourceSnapshot) {
    if (!source.source) return;
    try {
      await focusSource(project, source.source);
    } catch {
      // No backend (e.g. `vite dev`) — nothing to raise.
    }
  }

  function windowHeight(
    v: View,
    rowCount: number,
    sourceCount: number,
    contextRows: number,
  ): number {
    const compact =
      TITLE_H +
      Math.max(1, rowCount) * ROW_H +
      sourceCount * SOURCE_ROW_H +
      contextRows * CONTEXT_ROW_H +
      LIST_PAD;
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

  // Hydrate the widget appearance once; the Settings panel then drives it live.
  $effect(() => {
    loadAppearance();
  });

  // The dark tokens in app.css are class-based; mirror the store flag onto <html>.
  $effect(() => {
    document.documentElement.classList.toggle("dark", appearance.dark);
  });

  // Keep the window sized to the current view + live project rows + expanded sources.
  $effect(() => {
    resize(
      windowHeight(
        view,
        projects.length,
        expandedSourceCount,
        focus.snapshot.context ? 1 : 0,
      ),
    );
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

<div class="panel select-none" style="opacity: {appearance.opacity};">
  <!-- App title bar. The whole bar moves the window (data-tauri-drag-region). -->
  <header
    data-tauri-drag-region
    class="flex items-center justify-between gap-2 px-3 pt-3 pb-2"
  >
    <span class="inline-flex items-center gap-2">
      <BrandMark size={18} />
      <span
        style="color: var(--title-fg); font-weight: var(--title-weight); font-size: 15px;"
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
        calendar_view_day
      </button>
      <button
        class="msym"
        style="color: var(--fg3); font-size: 18px; cursor: pointer;"
        title="Close"
        onclick={() => getCurrentWindow().close()}
      >
        close
      </button>
    </div>
  </header>

  <!-- The roster: one row per live project, each expandable to its sources (§9), or a
       quiet idle row when nothing's live. -->
  <div class="pb-2">
    {#if projects.length === 0}
      <div
        class="flex items-center gap-2 px-3 py-1"
        style="opacity: 0.55; color: var(--fg3); font-size: 14px;"
      >
        <span class="inline-block shrink-0" style="width: 16px;"></span>
        idle
      </div>
    {:else}
      {#each projects as project (project.project)}
        <ProjectRow
          {project}
          context={project.active ? (focus.snapshot.context ?? null) : null}
          expanded={expanded.has(project.project)}
          onToggle={() => toggle(project.project)}
          onActivate={activateSource}
        />
      {/each}
    {/if}
  </div>

  {#if view !== "compact"}
    <div
      class="flex min-h-0 flex-1 flex-col overflow-hidden"
      style="border-top: 1px solid var(--border-soft);"
    >
      {#if view === "timeline"}
        <div class="wn-scroll overflow-y-auto pt-2">
          <BlockTimeline {blocks} />
        </div>
      {:else if view === "settings"}
        <SettingsPanel />
      {/if}
    </div>
  {/if}
</div>
