<script lang="ts">
  import type { FocusBlock } from "$lib/types";

  // Expanded state: today's blocks as a slim, proportional rail. Diagnostic only
  // — no judgement, no "you switched N times". Just where the day went.
  let { blocks }: { blocks: FocusBlock[] } = $props();

  // Roll same-project blocks up into totals for the summary line.
  const totals = $derived.by(() => {
    const m = new Map<string, number>();
    for (const b of blocks) m.set(b.project, (m.get(b.project) ?? 0) + (b.end - b.start));
    return [...m.entries()].sort((a, b) => b[1] - a[1]);
  });

  const grandTotal = $derived(totals.reduce((s, [, secs]) => s + secs, 0));

  function dur(secs: number): string {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    return h > 0 ? `${h}h${m.toString().padStart(2, "0")}m` : `${m}m`;
  }

  function clock(unix: number): string {
    return new Date(unix * 1000).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    });
  }
</script>

<div class="flex flex-col gap-2 px-3 pb-3">
  {#if blocks.length === 0}
    <p style="color: var(--fg3); font-size: 12px;">No blocks yet today.</p>
  {:else}
    <p class="label">
      {blocks.length} block{blocks.length === 1 ? "" : "s"} today
    </p>
    <!-- proportional rail -->
    <div class="flex h-1.5 w-full overflow-hidden rounded-full" style="background: var(--border-soft);">
      {#each blocks as b (b.start)}
        <div
          title={`${b.project} · ${dur(b.end - b.start)}`}
          style="width: {grandTotal > 0 ? ((b.end - b.start) / grandTotal) * 100 : 0}%; background: var(--accent); opacity: 0.85; border-right: 1px solid var(--bg);"
        ></div>
      {/each}
    </div>
    <!-- per-project totals -->
    <div class="flex flex-col gap-1">
      {#each totals as [project, secs] (project)}
        <div class="flex items-center justify-between" style="font-size: 12px;">
          <span class="truncate" style="color: var(--fg-body);">{project}</span>
          <span class="tabular-nums" style="color: var(--fg2);">{dur(secs)}</span>
        </div>
      {/each}
    </div>
    <!-- per-block rows: when + where, with the stored context string as the memory
         jogger (docs/context-strings.md §6). Blocks without one just show the span. -->
    <div
      class="flex flex-col gap-1"
      style="border-top: 1px solid var(--border-soft); padding-top: 8px;"
    >
      {#each blocks as b (b.start)}
        <div class="flex items-baseline gap-2" style="font-size: 12px;">
          <span class="tabular-nums shrink-0" style="color: var(--fg3);"
            >{clock(b.start)}–{clock(b.end)}</span
          >
          <span class="shrink-0" style="color: var(--fg-body);">{b.project}</span>
          {#if b.context}
            <span class="truncate" style="color: var(--fg3);" title={b.context.text}
              >{b.context.text}</span
            >
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>
