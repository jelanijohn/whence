<script lang="ts">
  import { onMount } from "svelte";
  import {
    getSettings,
    setSettings,
    installClaudeHooks,
    uninstallClaudeHooks,
  } from "$lib/tauri";
  import type { Settings } from "$lib/types";
  import Toggle from "./Toggle.svelte";

  // The settings form. Self-contained like BlockTimeline: loads its own state on
  // mount and writes through setSettings on Save. Edits stay local until Save —
  // disk write + the autostart launch-agent reconcile happen once, on click.

  // Editable shape: text overrides held as plain strings ('' = "use default" =
  // null on the wire), aliases as an ordered row list so the Record stays editable.
  type Draft = {
    autostart: boolean;
    neuroskillEnabled: boolean;
    neuroskillEndpoint: string;
    neuroskillTokenPath: string;
    neuroskillDataDir: string;
    ollamaEnabled: boolean;
    ollamaEndpoint: string;
    switchMinSeconds: number;
    idleTimeoutSeconds: number;
    aliases: { key: string; value: string }[];
  };

  let loading = $state(true);
  let saving = $state(false);
  let saved = $state(false);
  let error = $state<string | null>(null);
  // The last-saved snapshot, used to detect dirty state.
  let baseline = $state<Settings | undefined>();
  let draft = $state<Draft | undefined>();

  function toDraft(s: Settings): Draft {
    return {
      autostart: s.autostart,
      neuroskillEnabled: s.neuroskillEnabled,
      neuroskillEndpoint: s.neuroskillEndpoint ?? "",
      neuroskillTokenPath: s.neuroskillTokenPath ?? "",
      neuroskillDataDir: s.neuroskillDataDir ?? "",
      ollamaEnabled: s.ollamaEnabled,
      ollamaEndpoint: s.ollamaEndpoint ?? "",
      switchMinSeconds: s.switchMinSeconds,
      idleTimeoutSeconds: s.idleTimeoutSeconds,
      aliases: Object.entries(s.projectAliases ?? {}).map(([key, value]) => ({ key, value })),
    };
  }

  const clampSecs = (n: number): number => Math.max(1, Math.round(Number(n) || 1));

  // Build the wire payload: trim strings ('' → null), clamp seconds, drop blank
  // alias rows. This is also what dirty-detection compares against. Spread the
  // last-saved baseline first so settings this panel doesn't surface (e.g.
  // hookListenAddrOverride) survive a Save instead of being reset to default.
  function toSettings(d: Draft): Settings {
    const projectAliases: Record<string, string> = {};
    for (const { key, value } of d.aliases) {
      const k = key.trim();
      const v = value.trim();
      if (k && v) projectAliases[k] = v;
    }
    return {
      ...(baseline as Settings),
      autostart: d.autostart,
      neuroskillEnabled: d.neuroskillEnabled,
      neuroskillEndpoint: d.neuroskillEndpoint.trim() || null,
      neuroskillTokenPath: d.neuroskillTokenPath.trim() || null,
      neuroskillDataDir: d.neuroskillDataDir.trim() || null,
      ollamaEnabled: d.ollamaEnabled,
      ollamaEndpoint: d.ollamaEndpoint.trim() || null,
      projectAliases,
      switchMinSeconds: clampSecs(d.switchMinSeconds),
      idleTimeoutSeconds: clampSecs(d.idleTimeoutSeconds),
    };
  }

  // Order-independent canonical form so dirty-detection ignores alias key order.
  function canonical(s: Settings): string {
    const aliases = Object.keys(s.projectAliases)
      .sort()
      .map((k) => [k, s.projectAliases[k]]);
    return JSON.stringify({
      autostart: s.autostart,
      neuroskillEnabled: s.neuroskillEnabled,
      neuroskillEndpoint: s.neuroskillEndpoint ?? null,
      neuroskillTokenPath: s.neuroskillTokenPath ?? null,
      neuroskillDataDir: s.neuroskillDataDir ?? null,
      ollamaEnabled: s.ollamaEnabled,
      ollamaEndpoint: s.ollamaEndpoint ?? null,
      switchMinSeconds: s.switchMinSeconds,
      idleTimeoutSeconds: s.idleTimeoutSeconds,
      aliases,
    });
  }

  const dirty = $derived(
    draft && baseline ? canonical(toSettings(draft)) !== canonical(baseline) : false,
  );

  onMount(async () => {
    try {
      const s = await getSettings();
      baseline = s;
      draft = toDraft(s);
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

  function addAlias() {
    draft?.aliases.push({ key: "", value: "" });
  }
  function removeAlias(i: number) {
    draft?.aliases.splice(i, 1);
  }

  async function save() {
    if (!draft || saving) return;
    saving = true;
    error = null;
    saved = false;
    try {
      const result = await setSettings(toSettings(draft));
      baseline = result;
      draft = toDraft(result); // re-sync: blank alias rows / trimmed values fall away
      saved = true;
      setTimeout(() => (saved = false), 1800);
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }

  // Claude Code hooks — an immediate side effect (writes ~/.claude/settings.json),
  // independent of the draft/Save flow above. Opt-in, reversible.
  let hooksBusy = $state(false);
  let hooksMsg = $state<string | null>(null);
  let hooksErr = $state<string | null>(null);

  async function runHooks(action: () => Promise<void>, ok: string) {
    if (hooksBusy) return;
    hooksBusy = true;
    hooksMsg = null;
    hooksErr = null;
    try {
      await action();
      hooksMsg = ok;
      setTimeout(() => (hooksMsg = null), 2400);
    } catch (e) {
      hooksErr = String(e);
    } finally {
      hooksBusy = false;
    }
  }
</script>

<div class="flex h-full flex-col">
  {#if loading}
    <p class="px-3 py-2" style="color: var(--fg3); font-size: 12px;">Loading settings…</p>
  {:else if draft}
    <!-- Scrollable body; the Save bar below stays pinned. -->
    <div class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-3 py-2">
    <!-- General -->
    <div class="flex items-center justify-between">
      <span style="color: var(--fg-body); font-size: 13px;">Launch at login</span>
      <Toggle bind:checked={draft.autostart} label="Launch at login" />
    </div>
    <div class="flex items-center justify-between">
      <span style="color: var(--fg-body); font-size: 13px;">Write NeuroSkill labels</span>
      <Toggle bind:checked={draft.neuroskillEnabled} label="Write NeuroSkill labels" />
    </div>

    <!-- Segmenter -->
    <div class="flex flex-col gap-2" style="border-top: 1px solid var(--border-soft);" >
      <p class="label" style="margin-top: 8px;">Segmenter</p>
      <div class="flex items-center justify-between gap-2">
        <span style="color: var(--fg-body); font-size: 13px;">Switch confirm</span>
        <span class="inline-flex items-center gap-1">
          <input
            class="wn-input tabular-nums"
            style="width: 64px; text-align: right;"
            type="number"
            min="1"
            step="1"
            bind:value={draft.switchMinSeconds}
          />
          <span style="color: var(--fg3); font-size: 12px;">s</span>
        </span>
      </div>
      <div class="flex items-center justify-between gap-2">
        <span style="color: var(--fg-body); font-size: 13px;">Idle timeout</span>
        <span class="inline-flex items-center gap-1">
          <input
            class="wn-input tabular-nums"
            style="width: 64px; text-align: right;"
            type="number"
            min="1"
            step="1"
            bind:value={draft.idleTimeoutSeconds}
          />
          <span style="color: var(--fg3); font-size: 12px;">s</span>
        </span>
      </div>
    </div>

    <!-- NeuroSkill overrides -->
    <div class="flex flex-col gap-2" style="border-top: 1px solid var(--border-soft);">
      <p class="label" style="margin-top: 8px;">NeuroSkill</p>
      <label class="flex flex-col gap-1">
        <span style="color: var(--fg2); font-size: 12px;">Endpoint override</span>
        <input
          class="wn-input"
          type="text"
          placeholder="http://127.0.0.1:18444 (default)"
          bind:value={draft.neuroskillEndpoint}
        />
      </label>
      <label class="flex flex-col gap-1">
        <span style="color: var(--fg2); font-size: 12px;">Token path override</span>
        <input
          class="wn-input"
          type="text"
          placeholder="auto-resolve (native → WSL2 host)"
          bind:value={draft.neuroskillTokenPath}
        />
      </label>
      <label class="flex flex-col gap-1">
        <span style="color: var(--fg2); font-size: 12px;">Data dir override</span>
        <input
          class="wn-input"
          type="text"
          placeholder="activity.sqlite folder — auto (intensity meter)"
          bind:value={draft.neuroskillDataDir}
        />
      </label>
    </div>

    <!-- Ollama -->
    <div class="flex flex-col gap-2" style="border-top: 1px solid var(--border-soft);">
      <p class="label" style="margin-top: 8px;">Ollama</p>
      <div class="flex items-center justify-between">
        <span style="color: var(--fg-body); font-size: 13px;">Inference liveness</span>
        <Toggle bind:checked={draft.ollamaEnabled} label="Ollama inference liveness" />
      </div>
      <p style="color: var(--fg3); font-size: 11px;">
        Polls Ollama for active local inference — a status hint only, never a project switch.
      </p>
      {#if draft.ollamaEnabled}
        <label class="flex flex-col gap-1">
          <span style="color: var(--fg2); font-size: 12px;">Endpoint override</span>
          <input
            class="wn-input"
            type="text"
            placeholder="http://localhost:11434 (default)"
            bind:value={draft.ollamaEndpoint}
          />
        </label>
      {/if}
    </div>

    <!-- Claude Code hooks -->
    <div class="flex flex-col gap-2" style="border-top: 1px solid var(--border-soft);">
      <p class="label" style="margin-top: 8px;">Claude Code hooks</p>
      <p style="color: var(--fg3); font-size: 11px;">
        Show <span style="color: var(--fg2);">waiting on you</span> the instant Claude finishes a turn.
        Writes hooks into <span class="tabular-nums">~/.claude/settings.json</span> (opt-in, reversible).
      </p>
      <div class="flex items-center gap-1.5">
        <button
          type="button"
          style="background: var(--accent); color: #fff; font-size: 12px; font-weight: 600; padding: 5px 12px; border-radius: 7px; cursor: pointer;"
          class:opacity-50={hooksBusy}
          disabled={hooksBusy}
          onclick={() => runHooks(installClaudeHooks, "Hooks installed")}
        >
          Install hooks
        </button>
        <button
          type="button"
          style="color: var(--fg2); font-size: 12px; padding: 5px 12px; border-radius: 7px; border: 1px solid var(--border-soft); cursor: pointer;"
          class:opacity-50={hooksBusy}
          disabled={hooksBusy}
          onclick={() => runHooks(uninstallClaudeHooks, "Hooks removed")}
        >
          Remove
        </button>
      </div>
      {#if hooksErr}
        <span style="color: #d9544f; font-size: 11px;" title={hooksErr} class="truncate">{hooksErr}</span>
      {:else if hooksMsg}
        <span style="color: var(--accent); font-size: 11px;">{hooksMsg}</span>
      {/if}
    </div>

    <!-- Project aliases -->
    <div class="flex flex-col gap-2" style="border-top: 1px solid var(--border-soft);">
      <div class="flex items-center justify-between" style="margin-top: 8px;">
        <p class="label">Project aliases</p>
        <button
          class="msym"
          style="color: var(--fg3); font-size: 18px; cursor: pointer;"
          title="Add alias"
          onclick={addAlias}>add</button
        >
      </div>
      {#if draft.aliases.length === 0}
        <p style="color: var(--fg3); font-size: 11px;">
          Map a transcript dir-name to a canonical slug.
        </p>
      {:else}
        {#each draft.aliases as alias, i (alias)}
          <div class="flex items-center gap-1.5">
            <input
              class="wn-input"
              style="flex: 1; min-width: 0;"
              type="text"
              placeholder="dir-name"
              bind:value={alias.key}
            />
            <span style="color: var(--fg4); font-size: 12px;">→</span>
            <input
              class="wn-input"
              style="flex: 1; min-width: 0;"
              type="text"
              placeholder="slug"
              bind:value={alias.value}
            />
            <button
              class="msym"
              style="color: var(--fg4); font-size: 16px; cursor: pointer;"
              title="Remove"
              onclick={() => removeAlias(i)}>close</button
            >
          </div>
        {/each}
      {/if}
    </div>
    </div>

    <!-- Save bar — pinned footer, outside the scroll region. -->
    <div
      class="flex shrink-0 items-center justify-between gap-2 px-3"
      style="border-top: 1px solid var(--border-soft); padding-top: 10px; padding-bottom: 10px; background: var(--bg);"
    >
      <span style="font-size: 11px; min-width: 0;" class="truncate">
        {#if error}
          <span style="color: #d9544f;" title={error}>{error}</span>
        {:else if saved}
          <span style="color: var(--accent);">Saved</span>
        {:else if dirty}
          <span style="color: var(--fg3);">Unsaved changes</span>
        {/if}
      </span>
      <button
        type="button"
        style="background: var(--accent); color: #fff; font-size: 12px; font-weight: 600; padding: 5px 14px; border-radius: 7px; cursor: pointer;"
        class:opacity-50={!dirty || saving}
        disabled={!dirty || saving}
        onclick={save}
      >
        {saving ? "Saving…" : "Save"}
      </button>
    </div>
  {:else if error}
    <p style="color: #d9544f; font-size: 12px;">{error}</p>
  {/if}
</div>
