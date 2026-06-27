<script lang="ts">
  import type { SessionSnapshot } from "$lib/types";
  import BrandMark from "./BrandMark.svelte";
  import StatusDot from "./StatusDot.svelte";
  import BlockTimer from "./BlockTimer.svelte";

  // One dense line per live session: glyph (active only) · project · presence ·
  // status · timer. The active/focused project is full-strength; the rest are dimmed
  // so the eye lands on what you're actually deep on without hiding the others.
  let { session }: { session: SessionSnapshot } = $props();

  // Present vs running (§7) — a subtle qualifier on the active project only, so the
  // widget never implies your attention when only Claude's is on the work. running
  // means: still accruing real work, but you may have stepped away.
  const presenceTitle = $derived(
    session.presence === "running"
      ? "running — only autonomous activity since your last prompt"
      : "present — you've prompted recently",
  );
</script>

<div
  class="flex items-center gap-2 px-3 py-1"
  style="opacity: {session.active ? 1 : 0.55};"
  title={session.project}
>
  <!-- Glyph marks the active session; others get a same-width spacer so names align. -->
  {#if session.active}
    <BrandMark size={16} />
  {:else}
    <span class="inline-block shrink-0" style="width: 16px;"></span>
  {/if}
  <span
    class="truncate"
    class:font-semibold={session.active}
    style="color: {session.active ? 'var(--fg)' : 'var(--fg2)'}; font-size: 14px;"
    >{session.project}</span
  >
  {#if session.active && session.presence}
    <span
      class="shrink-0 tabular-nums"
      style="color: var(--fg3); font-size: 11px;"
      title={presenceTitle}>· {session.presence}</span
    >
  {/if}
  <span class="ml-auto flex shrink-0 items-center gap-3">
    <StatusDot status={session.status} />
    <BlockTimer start={session.blockStart} />
  </span>
</div>
