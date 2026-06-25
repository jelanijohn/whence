<script lang="ts">
  import type { Status } from "$lib/types";

  let { status }: { status: Status } = $props();

  // Status surfaces immediately (not debounced) — the dot is the most responsive
  // thing on the widget. awaiting_input pulses to pull the eye; idle is quiet.
  const color = $derived(
    status === "active"
      ? "var(--status-active)"
      : status === "awaiting_input"
        ? "var(--status-awaiting)"
        : "var(--status-idle)",
  );
  const label = $derived(
    status === "active"
      ? "active"
      : status === "awaiting_input"
        ? "awaiting you"
        : "idle",
  );
</script>

<span class="inline-flex items-center gap-1.5" title={label}>
  <span
    class="sdot"
    class:pulse={status === "awaiting_input"}
    style="--sc: {color};"
  ></span>
  <span class="label">{label}</span>
</span>
