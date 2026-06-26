<script lang="ts">
  import type { NeuroskillStatus } from "$lib/types";

  // The header's NeuroSkill connection indicator — a small dot, color-coded by the
  // live daemon health the backend probe reports. Diagnostic, never evaluative: it
  // shows whether the label write path is reachable, it never nags. Hidden while
  // status is still "unknown" (pre-probe) so the widget stays quiet on first paint.
  let { status }: { status: NeuroskillStatus } = $props();

  const color = $derived(
    status === "connected"
      ? "var(--status-active)"
      : status === "unauthorized"
        ? "var(--status-awaiting)"
        : status === "unreachable"
          ? "var(--status-error)"
          : "var(--fg4)", // disabled — muted, "off"
  );

  const title = $derived(
    status === "connected"
      ? "NeuroSkill connected"
      : status === "unauthorized"
        ? "NeuroSkill: auth token rejected (401)"
        : status === "unreachable"
          ? "NeuroSkill unreachable — is the daemon running?"
          : "NeuroSkill labels off",
  );
</script>

{#if status !== "unknown"}
  <span class="inline-flex items-center" {title} aria-label={title}>
    <span class="msym" style="color: {color}; font-size: 15px;">sensors</span>
  </span>
{/if}
