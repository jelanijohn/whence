<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import {
    getSettings,
    setSettings,
    installClaudeHooks,
    uninstallClaudeHooks,
    getBrowserMappingPath,
  } from "$lib/tauri";
  import type { Settings } from "$lib/types";
  import { neuroskill } from "$lib/stores/neuroskill.svelte";
  import { appearance, clampOpacity, MIN_OPACITY } from "$lib/stores/appearance.svelte";
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
    terminalEnabled: boolean;
    browserEnabled: boolean;
    switchMinSeconds: number;
    idleTimeoutSeconds: number;
    corroboratorConfidenceCutoff: number;
    attentionRecencySeconds: number;
    widgetOpacity: number;
    alwaysOnTop: boolean;
    alwaysPresent: boolean;
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
      terminalEnabled: s.terminalEnabled,
      browserEnabled: s.browserEnabled,
      switchMinSeconds: s.switchMinSeconds,
      idleTimeoutSeconds: s.idleTimeoutSeconds,
      corroboratorConfidenceCutoff: s.corroboratorConfidenceCutoff,
      attentionRecencySeconds: s.attentionRecencySeconds,
      widgetOpacity: s.widgetOpacity,
      alwaysOnTop: s.alwaysOnTop,
      alwaysPresent: s.alwaysPresent,
      aliases: Object.entries(s.projectAliases ?? {}).map(([key, value]) => ({ key, value })),
    };
  }

  const clampSecs = (n: number): number => Math.max(1, Math.round(Number(n) || 1));
  // Confidence is a 0..1 weight; clamp and keep two decimals of resolution.
  const clamp01 = (n: number): number =>
    Math.min(1, Math.max(0, Math.round((Number(n) || 0) * 100) / 100));

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
      terminalEnabled: d.terminalEnabled,
      browserEnabled: d.browserEnabled,
      projectAliases,
      switchMinSeconds: clampSecs(d.switchMinSeconds),
      idleTimeoutSeconds: clampSecs(d.idleTimeoutSeconds),
      corroboratorConfidenceCutoff: clamp01(d.corroboratorConfidenceCutoff),
      attentionRecencySeconds: clampSecs(d.attentionRecencySeconds),
      widgetOpacity: clampOpacity(d.widgetOpacity),
      alwaysOnTop: d.alwaysOnTop,
      alwaysPresent: d.alwaysPresent,
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
      terminalEnabled: s.terminalEnabled,
      browserEnabled: s.browserEnabled,
      switchMinSeconds: s.switchMinSeconds,
      idleTimeoutSeconds: s.idleTimeoutSeconds,
      corroboratorConfidenceCutoff: s.corroboratorConfidenceCutoff,
      attentionRecencySeconds: s.attentionRecencySeconds,
      widgetOpacity: s.widgetOpacity,
      alwaysOnTop: s.alwaysOnTop,
      alwaysPresent: s.alwaysPresent,
      aliases,
    });
  }

  const dirty = $derived(
    draft && baseline ? canonical(toSettings(draft)) !== canonical(baseline) : false,
  );

  // Opacity is the one setting with an immediate visual effect, so we live-preview
  // it: mirror the draft into the appearance store (which drives the real `.panel`)
  // while this panel is open, and restore the saved value on close so an unsaved
  // drag reverts. Save persists baseline, so a saved change sticks past unmount.
  $effect(() => {
    if (draft) appearance.opacity = clampOpacity(draft.widgetOpacity);
  });
  onDestroy(() => {
    if (baseline) appearance.opacity = clampOpacity(baseline.widgetOpacity);
  });

  // Live NeuroSkill connection health (backend probe) — the in-panel echo of the
  // header indicator, so you can see the effect of an endpoint/token edit here.
  const connColor = $derived(
    neuroskill.status === "connected"
      ? "var(--status-active)"
      : neuroskill.status === "unauthorized"
        ? "var(--status-awaiting)"
        : neuroskill.status === "unreachable"
          ? "var(--status-error)"
          : "var(--fg4)",
  );
  const connLabel = $derived(
    neuroskill.status === "connected"
      ? "Connected"
      : neuroskill.status === "unauthorized"
        ? "Auth token rejected (401)"
        : neuroskill.status === "unreachable"
          ? "Unreachable — is the daemon running?"
          : neuroskill.status === "disabled"
            ? "Off"
            : "Checking…",
  );

  // The hand-editable browser mapping file's path — surfaced as a deep-link when the
  // browser adapter is on. Best-effort: a failure just hides the path hint.
  let browserMappingPath = $state<string | null>(null);

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
    try {
      browserMappingPath = await getBrowserMappingPath();
    } catch {
      browserMappingPath = null;
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
    <div class="wn-scroll flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-3 py-2">
    <!-- General -->
    <div class="flex items-center justify-between">
      <span style="color: var(--fg-body); font-size: 13px;">Launch at login</span>
      <Toggle bind:checked={draft.autostart} label="Launch at login" />
    </div>

    <!-- Window — float-on-top + cross-desktop presence. Applied on Save (the
         set_settings side-effect path), mirroring autostart. -->
    <div class="flex flex-col gap-2" style="border-top: 1px solid var(--border-soft);">
      <p class="label" style="margin-top: 8px;">Window</p>
      <div class="flex items-center justify-between">
        <span style="color: var(--fg-body); font-size: 13px;">Always on top</span>
        <Toggle bind:checked={draft.alwaysOnTop} label="Always on top" />
      </div>
      <div class="flex items-center justify-between">
        <span style="color: var(--fg-body); font-size: 13px;">Always present</span>
        <Toggle bind:checked={draft.alwaysPresent} label="Always present" />
      </div>
      <p style="color: var(--fg3); font-size: 11px;">
        Keeps the widget in view when you switch virtual desktops. macOS and Linux
        (GNOME/libunity) only — no effect on Windows.
      </p>
    </div>

    <!-- Appearance — whole-widget opacity. A constant value (not adaptive), so the
         widget stays glanceable; floored at 30% so it never goes unreadable. The
         slider live-previews as it moves (see the $effect above). -->
    <div class="flex flex-col gap-2" style="border-top: 1px solid var(--border-soft);">
      <p class="label" style="margin-top: 8px;">Appearance</p>
      <div class="flex items-center justify-between gap-3">
        <span style="color: var(--fg-body); font-size: 13px;" title="Whole-widget opacity. Lower = more see-through; floored at 30% so it stays readable.">Widget opacity</span>
        <span class="inline-flex items-center gap-2" style="flex: 1; max-width: 168px;">
          <input
            class="wn-range"
            style="flex: 1; min-width: 0;"
            type="range"
            min={MIN_OPACITY}
            max="1"
            step="0.05"
            bind:value={draft.widgetOpacity}
          />
          <span class="tabular-nums" style="color: var(--fg3); font-size: 12px; width: 34px; text-align: right;"
            >{Math.round(draft.widgetOpacity * 100)}%</span
          >
        </span>
      </div>
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
      <div class="flex items-center justify-between gap-2">
        <span style="color: var(--fg-body); font-size: 13px;" title="Attention-recency window: how recently you must have prompted for the block to read “present” rather than “running”.">Attention window</span>
        <span class="inline-flex items-center gap-1">
          <input
            class="wn-input tabular-nums"
            style="width: 64px; text-align: right;"
            type="number"
            min="1"
            step="1"
            bind:value={draft.attentionRecencySeconds}
          />
          <span style="color: var(--fg3); font-size: 12px;">s</span>
        </span>
      </div>
      <div class="flex items-center justify-between gap-2">
        <span style="color: var(--fg-body); font-size: 13px;" title="Confidence at/above which a signal is primary (can switch focus); below it it only corroborates.">Weak-hint cutoff</span>
        <span class="inline-flex items-center gap-1">
          <input
            class="wn-input tabular-nums"
            style="width: 64px; text-align: right;"
            type="number"
            min="0"
            max="1"
            step="0.05"
            bind:value={draft.corroboratorConfidenceCutoff}
          />
          <span style="color: var(--fg3); font-size: 12px;">conf</span>
        </span>
      </div>
    </div>

    <!-- NeuroSkill — toggle heads the section (like Ollama); the connection
         overrides reveal only when label-writing is on. -->
    <div class="flex flex-col gap-2" style="border-top: 1px solid var(--border-soft);">
      <p class="label" style="margin-top: 8px;">NeuroSkill</p>
      <div class="flex items-center justify-between">
        <span style="color: var(--fg-body); font-size: 13px;">Write attribution labels</span>
        <Toggle bind:checked={draft.neuroskillEnabled} label="Write NeuroSkill labels" />
      </div>
      <!-- Live connection health from the backend probe — echoes the header dot. -->
      <div class="flex items-center gap-1.5">
        <span class="sdot" style="--sc: {connColor};"></span>
        <span style="color: var(--fg2); font-size: 11px;">{connLabel}</span>
      </div>
      {#if draft.neuroskillEnabled}
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
      {/if}
      <!-- Data dir powers the read-only intensity meter, independent of the label
           write path — so it stays visible regardless of the toggle. -->
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

    <!-- Terminal cwd -->
    <div class="flex flex-col gap-2" style="border-top: 1px solid var(--border-soft);">
      <p class="label" style="margin-top: 8px;">Terminal</p>
      <div class="flex items-center justify-between">
        <span style="color: var(--fg-body); font-size: 13px;">Shell cwd hints</span>
        <Toggle bind:checked={draft.terminalEnabled} label="Terminal cwd hints" />
      </div>
      <p style="color: var(--fg3); font-size: 11px;">
        Corroborates the current project from your shell's directory — never switches focus on its own.
      </p>
      {#if draft.terminalEnabled}
        <p style="color: var(--fg3); font-size: 11px;">
          Add a shell hook POSTing <span class="tabular-nums">$PWD</span> to
          <span class="tabular-nums">127.0.0.1:18451/cwd</span>. zsh:
          <code style="color: var(--fg2); font-size: 10px; word-break: break-all;"
            >{`chpwd(){ curl -sm1 -d "{\\"cwd\\":\\"$PWD\\"}" 127.0.0.1:18451/cwd >/dev/null 2>&1 }`}</code
          >
        </p>
      {/if}
    </div>

    <!-- Browser LLM -->
    <div class="flex flex-col gap-2" style="border-top: 1px solid var(--border-soft);">
      <p class="label" style="margin-top: 8px;">Browser</p>
      <div class="flex items-center justify-between">
        <span style="color: var(--fg-body); font-size: 13px;">Browser AI sessions</span>
        <Toggle bind:checked={draft.browserEnabled} label="Browser LLM sessions" />
      </div>
      <p style="color: var(--fg3); font-size: 11px;">
        Attributes claude.ai / chatgpt.com chats by their provider project — a first-class session,
        not a corroborator. Needs the Whence browser extension.
      </p>
      {#if draft.browserEnabled}
        <p style="color: var(--fg3); font-size: 11px;">
          Install the extension from <span class="tabular-nums">extension/</span> (load unpacked); it
          POSTs to <span class="tabular-nums">127.0.0.1:18452/browser</span>.
        </p>
        {#if browserMappingPath}
          <p style="color: var(--fg3); font-size: 11px;">
            Project mapping (hand-editable):
            <code style="color: var(--fg2); font-size: 10px; word-break: break-all;"
              >{browserMappingPath}</code
            >
          </p>
        {/if}
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
