<script lang="ts">
  // A small bindable switch for boolean settings. Bindable so callers can use
  // `bind:checked` and read the value back without an event dance.
  let {
    checked = $bindable(false),
    label,
    disabled = false,
  }: { checked?: boolean; label: string; disabled?: boolean } = $props();
</script>

<button
  type="button"
  role="switch"
  aria-checked={checked}
  aria-label={label}
  {disabled}
  class="toggle"
  class:on={checked}
  onclick={() => (checked = !checked)}
>
  <span class="toggle-knob"></span>
</button>
