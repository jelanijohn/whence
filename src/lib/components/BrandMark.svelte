<script lang="ts">
  // The green "W" brand mark, shared across the desktop trio. Rendered via CSS
  // mask so the same file recolors per surface. Defaults to the fixed brand green.
  let {
    size = 18,
    color = "var(--wn-brand-green)",
  }: {
    size?: number;
    color?: string;
  } = $props();
</script>

<span
  aria-hidden="true"
  class="inline-block shrink-0"
  style="width: {size}px; height: {size}px; background: {color}; -webkit-mask: url(/logo.svg) center / contain no-repeat; mask: url(/logo.svg) center / contain no-repeat;"
></span>
