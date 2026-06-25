<script lang="ts">
  // Ticks the "how long on this block" readout once a second off the open block's
  // start. Self-contained interval so the parent doesn't re-render on every tick.
  let { start }: { start: number | null } = $props();

  let now = $state(Math.floor(Date.now() / 1000));
  $effect(() => {
    const id = setInterval(() => (now = Math.floor(Date.now() / 1000)), 1000);
    return () => clearInterval(id);
  });

  const text = $derived.by(() => {
    if (start == null) return "—";
    const secs = Math.max(0, now - start);
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    if (h > 0) return `${h}h${m.toString().padStart(2, "0")}m`;
    if (m > 0) return `${m}m`;
    return `${secs}s`;
  });
</script>

<span class="tabular-nums" style="color: var(--fg2); font-size: 12px;">{text}</span>
