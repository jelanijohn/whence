<script lang="ts">
  import type { ContextString, ProjectSnapshot, SourceSnapshot } from "$lib/types";
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
    context = null,
    expanded,
    onToggle,
    onActivate,
  }: {
    project: ProjectSnapshot;
    // The focused project's context string (git branch · commit) — set only on the
    // active row; absent = no line, no reserved height (docs/context-strings.md §6).
    context?: ContextString | null;
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

  <!-- Context string (docs/context-strings.md §6): a muted secondary line under the
       project name — what you were doing there, to jog memory. Indented to align
       with the name (12px pad + 16px glyph + 8px gap). Display-only; absent when
       nothing resolved. -->
  {#if context}
    <div
      class="truncate"
      style="padding: 0 12px 2px 36px; color: var(--fg3); font-size: 11px; line-height: 14px;"
      title={context.text}
    >
      {context.text}
    </div>
  {/if}

  {#if expanded}
    {#each project.sources as source (source.source ?? source.label)}
      <SourceRow {source} onActivate={() => onActivate?.(project.project, source)} />
    {/each}
  {/if}
</div>
