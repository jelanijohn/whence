<script lang="ts">
  import type { ProjectSnapshot, SourceSnapshot } from "$lib/types";
  import BrandMark from "./BrandMark.svelte";
  import StatusDot from "./StatusDot.svelte";
  import BlockTimer from "./BlockTimer.svelte";
  import SourceRow from "./SourceRow.svelte";

  // One project row in the roster (§9): glyph (active only) · project · presence ·
  // status · state timer · expand chevron. The active/focused project is full-strength;
  // the rest are dimmed so the eye lands on what you're actually deep on without hiding
  // what else is running or waiting on you. Expanding reveals the project's live sources.
  let {
    project,
    expanded,
    onToggle,
    onActivate,
  }: {
    project: ProjectSnapshot;
    expanded: boolean;
    onToggle: () => void;
    // Fired when a (browser) source row is clicked: raise its tab + focus the project.
    onActivate?: (project: string, source: SourceSnapshot) => void;
  } = $props();

  // Present vs running (§7) — a subtle qualifier on the active project only, so the
  // widget never implies your attention when only Claude's is on the work.
  const presenceTitle = $derived(
    project.presence === "running"
      ? "running — only autonomous activity since your last prompt"
      : "present — you've prompted recently",
  );
</script>

<div>
  <div
    class="flex items-center gap-2 px-3 py-1"
    style="opacity: {project.active ? 1 : 0.55};"
    title={project.project}
  >
    <!-- Glyph marks the focus project; others get a same-width spacer so names align. -->
    {#if project.active}
      <BrandMark size={16} />
    {:else}
      <span class="inline-block shrink-0" style="width: 16px;"></span>
    {/if}
    <span
      class="truncate"
      class:font-semibold={project.active}
      style="color: {project.active ? 'var(--fg)' : 'var(--fg2)'}; font-size: 14px;"
      >{project.project}</span
    >
    {#if project.active && project.presence}
      <span
        class="shrink-0 tabular-nums"
        style="color: var(--fg3); font-size: 11px;"
        title={presenceTitle}>· {project.presence}</span
      >
    {/if}
    <span class="ml-auto flex shrink-0 items-center gap-3">
      <StatusDot status={project.status} />
      <BlockTimer start={project.statusSince} />
      {#if project.sources.length > 0}
        <button
          class="msym"
          style="color: {expanded ? 'var(--accent)' : 'var(--fg3)'}; font-size: 16px; cursor: pointer;"
          title={expanded ? "Hide sources" : "Show sources"}
          onclick={onToggle}
        >
          {expanded ? "expand_less" : "expand_more"}
        </button>
      {/if}
    </span>
  </div>

  {#if expanded}
    {#each project.sources as source (source.source ?? source.label)}
      <SourceRow {source} onActivate={() => onActivate?.(project.project, source)} />
    {/each}
  {/if}
</div>
