<script lang="ts">
  import BrandMark from "./BrandMark.svelte";

  // The current-focus headline: brand glyph + project name. A quiet
  // "unattributed" state rather than going blank when nothing is attributed.
  let { project }: { project: string | null } = $props();
</script>

<span class="inline-flex items-center gap-2 min-w-0">
  <BrandMark size={18} />
  {#if project}
    <span
      class="truncate font-semibold"
      style="color: var(--fg); font-size: 15px;"
      title={project}>{project}</span
    >
  {:else}
    <span class="truncate" style="color: var(--fg3); font-size: 15px;"
      >unattributed</span
    >
  {/if}
</span>
