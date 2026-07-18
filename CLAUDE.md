# Whence — Working Guidance for Claude Code

Whence is an **ambient focus widget + local attribution sensor**. It infers which
project you're working on from your AI-tool activity, shows it in a small
always-on-top widget, and writes attribution labels into NeuroSkill so EEG data
downstream knows *what* you were deep on. It is a **producer, a peer to
NeuroSkill** — not a consumer. See `whence-spec.md` for the full design.

## Commands

```bash
pnpm install              # first-time setup
pnpm tauri dev            # run the widget (frontend + Rust backend)
pnpm tauri:wsl            # same, with WEBKIT_DISABLE_DMABUF_RENDERER=1 for WSL2
pnpm check                # svelte-check (frontend types)
cd src-tauri && cargo test            # engine + adapter + store unit tests
cd src-tauri && cargo test --features eeg-readback   # include the SQLite read-back
```

The dev server runs on **port 1425** (WAID uses 1420, so both widgets can run at
once).

## Architecture (mirrors the spec §4)

```
adapters/  →  engine/segment.rs  →  outputs (widget · NeuroSkill labels · timeline)
```

* **`src-tauri/src/adapters/`** — per-surface, pure-ish. Each translates a
  surface's native signal into a normalized `WorkEvent` and sends it on the core
  channel. All are implemented: `claude_code.rs` (transcript watch) and
  `hooks.rs` (the Claude Code hook receiver, a `tiny_http` listener on
  `127.0.0.1:18450` for live `awaiting_input` status) run by default; `ollama.rs`
  (`/api/ps` liveness) and `terminal.rs` (cwd corroborator on `127.0.0.1:18451`)
  are opt-in, default OFF (`ollama_enabled` / `terminal_enabled`), and only
  spawned when enabled. Adding a surface = adding an adapter; nothing else changes.
* **`src-tauri/src/auth.rs`** — receiver auth. All three loopback receivers (hooks
  `18450`, terminal `18451`, browser `18452` incl. `GET /raise`) are **bearer-gated**
  by a per-install token: minted at first launch, stored `0600` as
  `<app_data_dir>/receiver.token`, shown/rotatable in Settings, applied live via a
  shared handle (`SharedToken`). Carriers: the installed hook URL embeds it
  (`/hook/<token>` — `http` hooks can't set headers); the terminal snippet and the
  extension (options page → `chrome.storage.local`) send `Authorization: Bearer`.
  Unauthenticated requests get 401 and bump a visible denial counter (diagnostic,
  never evaluative). The request check (`request_authorized`) is pure and
  fixture-tested — keep the sockets the only impure part.
* **`src-tauri/src/engine/segment.rs`** — the heart. **Pure and fixture-tested**:
  no I/O, no clock reads, every time comes in via the event or an explicit `now`.
  This is the debounce/switch-confirmation/block logic. *Keep it pure* — if you
  need the wall clock or a file, do it in `orchestrator.rs` and pass the result
  in. Feed it a `WorkEvent` sequence, assert the blocks.
* **`src-tauri/src/engine/timeline.rs`** — the local store. **JSONL, one block per
  line** (see the module doc for why JSONL beats SQLite for *this* workload).
* **`src-tauri/src/neuroskill/`** — the write path. `client.rs` fires the `label`
  command over the daemon's **HTTP** API (`POST /`, bearer-token gated), lifted
  from WAID's `neuroskill/client.rs`. `eeg.rs` is the optional read-only
  intensity read-back (behind the `eeg-readback` feature).
  * **Token resolution is WSL2-aware** (`client::resolve_token_path`). Order:
    explicit `neuroskill_token_path` setting → native `<config>/skill/daemon/auth.token`
    if it exists → **WSL2 Windows-host discovery** (walks
    `/mnt/<drive>/Users/<user>/AppData/Roaming/skill/daemon/auth.token`). Under
    WSL2 the daemon runs on the Windows host, so the Linux config dir has no token
    and an unauthenticated call 401s — same split WAID hit. The daemon is reached
    at `127.0.0.1:18444` thanks to `.wslconfig` mirrored networking. Endpoint and
    token path are both overridable via `Settings` (`neuroskill_endpoint`,
    `neuroskill_token_path`); the token is read at call time so rotation/discovery
    needs no restart.
* **`src-tauri/src/orchestrator.rs`** — wires adapters → segmenter → outputs. The
  *only* place allowed to be impure around the engine.
* **`src/`** — the SvelteKit widget (Svelte 5 runes, SPA, Tailwind v4). Reuses
  WAID's "Tidewater · Refined" tokens and `BrandMark`.

## Non-negotiable principles (from the spec §3)

1. **Semantic work events, never raw window/OS focus.** Read what you produce
   (prompts, cwd, session lifecycle), never which window is foregrounded.
2. **Piggyback — no capture interrupts.** Whence never asks "what are you doing?".
3. **Diagnostic, never evaluative.** The widget reports state; it never scolds you
   for switching. No "you fragmented 9 times".
4. **Local-first, no backend, no cloud, no auth.**
5. **Read-scoped, bounded writes.** The *only* thing written into NeuroSkill is the
   session label. The EEG read-back is read-only (`mode=ro&immutable=1`). See the
   read-scope guard test in `neuroskill/eeg.rs`.
6. **Promote-gated downstream.** Whence *proposes*; humans promote. (v2.)

## Key conventions

* **Project slug** is the join key across the ecosystem — it must match WAID's
  brief slug and Who Am I's naming. Resolved in `adapters/claude_code::resolve_slug`,
  in priority order: **(1)** a `project_aliases` override keyed by transcript dir
  name; **(2)** the basename of the *first* `cwd` line in the active transcript —
  the lossless source, read once per dir and cached; **(3)** the dir-name trailing
  segment (`slug_from_transcript_dir`) as a provisional fallback. The dir name's
  `/`→`-` encoding is ambiguous (e.g. `glue-mac` → `mac`, `one-domino-square` →
  `square`), so cwd is preferred. Caveat: a transcript *synced from another
  machine* carries that machine's cwd — only affects historical files, since the
  adapter reads the changed (local) file; pin such cases with an alias.
* **NeuroSkill label namespace:** `Whence:project=<slug>:start|end` — source-
  namespaced so it never collides with WAID's manual `waid:brief=<slug>:…`.
  Downstream prefers the manual `waid:` label on conflict.
* **TS ↔ Rust models** must stay in sync: `src/lib/types.ts` mirrors
  `adapters/mod.rs` + `engine/segment.rs`. serde reps must match field-for-field
  (`kind` is snake_case, `surface` is kebab-case, `FocusBlock`/`FocusSnapshot`
  fields are camelCase).
* **Status vs focus (§7):** status (`active`/`awaiting_input`/`idle`) surfaces
  *immediately* to the widget and never moves a block boundary; focus switches go
  through the debounce. Both live in `segment.rs`.

## Plugins

* **`tauri-plugin-single-instance`** — registered FIRST (Tauri requirement). A
  sensor must not run twice and double-write labels.
* **`tauri-plugin-window-state`** — the widget remembers its position.
* **`tauri-plugin-autostart`** — **opt-in only**, default OFF, never default-on.
  Toggled via the `autostart` setting; `commands::set_settings` reconciles it with
  the OS launch agent only when it changes.

## Build phases (spec §13)

* **v0 (done):** Tauri+Svelte shell, transcript watcher, widget shows current
  focus + status + block timer, JSONL timeline, NeuroSkill label write.
* **v1 (done):** debounce, idle, JSONL timeline, label write, compact + expanded
  widget all ship. The segmentation upgrade landed — present-vs-running (the active
  block reads `present` while you've acted within the attention-recency window, else
  `running`; the widget shows `· present`/`· running` on the focus row), the
  three-tier protect/yield trust model (you-acted `Prompt`/`SessionStart` switch
  immediately and mark the block *present*; autonomous `ToolUse`/`Active` only build
  a switch candidate once the block has gone *running*; a sub-cutoff weak hint
  reinforces, and on a *running* block a stray hint elsewhere *drops* it), and the
  two new calibration knobs (`corroborator_confidence_cutoff` ~0.6,
  `attention_recency_seconds` ~120) — all on `engine/segment.rs` + `settings.rs` +
  `src/lib/types.ts` + widget, fixture-tested. Remaining: debounce calibration on
  real data.
* **v1.5 (done):** Claude Code hooks receiver (real-time `awaiting_input` status),
  Ollama liveness, optional terminal cwd, EEG read-back intensity meter, settings
  UI — all built (Ollama/terminal opt-in, default OFF). NeuroSkill connection-health
  indicator added as a bonus.
* **v2:** Who Am I inbox candidates; WAID intention-vs-reality.
* **v2 (optional):** thread an aggregated semantic `detail` summary into
  `FocusBlock` so a Who Am I connector ingesting `timeline.jsonl` can draft *what*
  you did, not just *how long* (the block carries time-attribution today, but
  `WorkEvent.detail` is dropped at block close). Touches `engine/segment.rs` (carry
  detail through the open block), `engine/timeline.rs` (schema), `src/lib/types.ts`
  (keep TS in sync). Ingestion contract: Who Am I reads `<app_data_dir>/timeline.jsonl`
  directly; dedupe on `(project, start)`.

## Style

TypeScript: strict, 2-space, single quotes. Rust: standard rustfmt. Match the
surrounding code's comment density — this codebase documents the *why*.
