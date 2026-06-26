<script lang="ts">
  // Optional EEG intensity read-back (spec §8) — a thin focus meter shown beside
  // attribution, closing the loop visually: *deep (EEG) on <project> (attribution)*.
  // Renders nothing when `value` is null (read-back feature off, no daemon store,
  // or no recent epochs) so the compact widget stays quiet. Diagnostic, never
  // evaluative — it shows intensity, it never scolds a "low focus" reading.
  let { value }: { value: number | null } = $props();

  const pct = $derived(value == null ? 0 : Math.max(0, Math.min(100, value)));
</script>

{#if value != null}
  <span class="inline-flex items-center gap-1.5" title="EEG focus intensity (NeuroSkill)">
    <span class="msym" style="color: var(--fg3); font-size: 13px;">neurology</span>
    <span class="meter"><span class="fill" style="width: {pct}%;"></span></span>
    <span class="tabular-nums" style="color: var(--fg2); font-size: 11px;">{Math.round(pct)}</span>
  </span>
{/if}

<style>
  .meter {
    position: relative;
    width: 36px;
    height: 4px;
    border-radius: 999px;
    background: var(--border-soft);
    overflow: hidden;
  }
  .fill {
    position: absolute;
    inset: 0 auto 0 0;
    height: 100%;
    border-radius: 999px;
    background: var(--accent);
    transition: width 0.6s ease;
  }
</style>
