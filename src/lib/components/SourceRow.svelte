<script lang="ts">
  import type { SourceSnapshot } from "$lib/types";
  import StatusDot from "./StatusDot.svelte";
  import BlockTimer from "./BlockTimer.svelte";

  // One source under an expanded project row (§9): a single Claude Code session,
  // browser conversation, or terminal — its surface label, its own status, its own
  // (time-in-status) timer. Indented under the project so three concurrent sessions
  // read as three lines under one project, not one blurred row.
  let { source }: { source: SourceSnapshot } = $props();
</script>

<div
  class="flex items-center gap-2 py-0.5 pr-3 pl-9"
  style="opacity: 0.8;"
  title={source.label}
>
  <span class="truncate" style="color: var(--fg3); font-size: 12px;">{source.label}</span>
  <span class="ml-auto flex shrink-0 items-center gap-3">
    <StatusDot status={source.status} />
    <BlockTimer start={source.statusSince} />
  </span>
</div>
