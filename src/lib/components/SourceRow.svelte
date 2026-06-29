<script lang="ts">
  import type { SourceSnapshot } from "$lib/types";
  import StatusDot from "./StatusDot.svelte";
  import BlockTimer from "./BlockTimer.svelte";

  // One source under an expanded project row (§9): a single Claude Code session,
  // browser conversation, or terminal — its surface label, its own status, its own
  // (time-in-status) timer. Indented under the project so three concurrent sessions
  // read as three lines under one project, not one blurred row.
  //
  // Browser rows are clickable: a click raises that tab and pulls its project into
  // focus (`onActivate`). Other surfaces have no raisable window on this platform, so
  // they stay inert — no cursor, no handler.
  let { source, onActivate }: {
    source: SourceSnapshot;
    onActivate?: () => void;
  } = $props();

  const clickable = $derived(
    source.surface === "browser" && !!source.source && !!onActivate,
  );

  function onKey(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onActivate?.();
    }
  }
</script>

{#snippet body()}
  <span class="truncate" style="color: var(--fg3); font-size: 12px;">{source.label}</span>
  <span class="ml-auto flex shrink-0 items-center gap-3">
    <StatusDot status={source.status} />
    <BlockTimer start={source.statusSince} />
  </span>
{/snippet}

{#if clickable}
  <div
    class="flex cursor-pointer items-center gap-2 py-0.5 pr-3 pl-9 opacity-80 transition-opacity hover:opacity-100"
    role="button"
    tabindex="0"
    title="Raise this tab · focus this project"
    onclick={onActivate}
    onkeydown={onKey}
  >
    {@render body()}
  </div>
{:else}
  <div
    class="flex items-center gap-2 py-0.5 pr-3 pl-9"
    style="opacity: 0.8;"
    title={source.label}
  >
    {@render body()}
  </div>
{/if}
