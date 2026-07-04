# Whence

An ambient focus widget and **local attribution sensor**. Whence infers which
project you're working on right now from your AI-tool activity, shows it in a
small always-on-top widget, and writes attribution labels into NeuroSkill so the
EEG data downstream knows *what* you were deep on — not just *how* deep.

> Whence is a **producer, a peer to NeuroSkill — not a consumer.** NeuroSkill
> measures focus *intensity* (how deep, via EEG); Whence measures focus
> *attribution* (deep on *what*). See [`whence-spec.md`](whence-spec.md) for the
> full design and the framing.

The whole design turns on one constraint: **attribution without surveillance.**
Whence reads the work artifacts you already produce — prompts, cwd, session
lifecycle — never which window is foregrounded. It piggybacks on actions you were
already taking; it never asks "what are you doing?", and it is diagnostic, never
evaluative (no "you switched 9 times"). Local-first: no backend, no cloud, no
auth.

---

## How it works

```
adapters/  →  engine/segment.rs  →  outputs (widget · NeuroSkill labels · timeline)
```

A surface adapter turns a native signal into a normalized `WorkEvent`. The
**segmentation engine** debounces those events into focus *blocks* — deciding
when a switch *actually* happened (a 30-second glance at another repo is not a
context switch; an hour is). Each confirmed block drives three outputs at once:
the widget, a NeuroSkill label write, and the local timeline.

The intelligence is in *not* flickering. A switch is confirmed only when a
candidate project clears a sustained-evidence threshold (`switch_min_seconds`); a
quiet gap longer than `idle_timeout_seconds` ends the block.

How fast a competing project can pull focus depends on *who* produced the
evidence. **You acting** — a prompt or a session start — is explicit intent: it
switches immediately and marks the block *present*. **Claude working on its own**
— autonomous transcript growth — only builds a switch candidate once the current
block has gone *running* (you've not acted within `attention_recency_seconds`),
so a background task can't yank you off a project you're actively prompting on. A
**weak hint** below `corroborator_confidence_cutoff` (a terminal `cd`) only
reinforces the current block — except that a *running* block, which you may have
stepped away from, yields to a single stray hint pointing elsewhere and drops. The
widget marks which mode the focus block is in (`· present` vs `· running`) so it
never implies your attention when only Claude's is on the work.

**Status** (`active` / `awaiting_input` / `idle`) is split out from focus — it
surfaces to the widget *immediately* and never moves a block boundary, so the
widget can light up the instant Claude Code is waiting on you without risking a
false switch.

Whence tracks *every* live project at once, not only the focused one. The widget
is a **roster**: one row per project — status rolled up by attention priority
(active → awaiting you → idle), the focus project pinned on top — and each row
expands to its live **sources**, so a project with three concurrent sessions reads
as three lines (each Claude Code session, browser conversation, or terminal, with
its own status and timer) rather than one blurred row. Only the single focused
project is *attributed*, though — it alone drives the NeuroSkill label and the
timeline, so persisted blocks never overlap.

---

## Architecture

```
src/                          SvelteKit widget (Svelte 5 runes, SPA, Tailwind v4).
  routes/                       +layout · the widget view (+page.svelte).
  lib/
    tauri.ts                    The only place naming backend commands + events.
    types.ts                    WorkEvent / FocusBlock / ProjectSnapshot +
                                  SourceSnapshot / FocusSnapshot / Settings — mirrors
                                  the Rust serde reps field-for-field.
    stores/                     Focus + NeuroSkill-connection state (runes), each
                                  fed by its event (focus · neuroskill).
    components/                 FocusBadge · StatusDot · ProjectRow · SourceRow ·
                                  BlockTimer · BlockTimeline · IntensityMeter ·
                                  NeuroskillStatusDot · SettingsPanel · Toggle · BrandMark.
src-tauri/src/
  lib.rs                        Plugin + command + window registration; spawns core.
  commands.rs                   get_focus_state · focus_source · get_today_blocks ·
                                  get_focus_intensity · get_neuroskill_status ·
                                  get_browser_mapping_path · get/set_settings ·
                                  install/uninstall_claude_hooks.
  orchestrator.rs               Wires adapters → segmenter → outputs (the impure seam).
  settings.rs                   Tiny JSON settings file in the app data dir.
  tray.rs                       System tray: restore the widget (left-click) + Quit
                                  (right-click menu) — the only un-hide path for the
                                  decorationless, skip-taskbar widget.
  adapters/
    mod.rs                      WorkEvent model (incl. per-instance source id) +
                                  adapter contract + shared slugify.
    claude_code.rs              Transcript watch (a live surface) + slug resolve.
    hooks.rs                    Loopback receiver for Claude Code http hooks —
                                  live awaiting_input status. PURE event mapping.
    ollama.rs                   /api/ps inference-liveness poll (low-confidence
                                  status only). PURE activity detection.
    terminal.rs                 Loopback receiver for shell cwd hints — a
                                  low-confidence corroborator. PURE cwd mapping.
    browser.rs                  Loopback receiver for the browser extension —
                                  originating-capable LLM-session attribution.
    browser_map.rs              Provider→slug mapping store (format-preserving TOML).
  engine/
    segment.rs                  Per-project registry (sources per project) +
                                  debounce / switch confirmation / blocks — PURE,
                                  fixture-tested: no I/O, every time comes in via
                                  the event or an explicit `now`.
    timeline.rs                 Local store: JSONL, one focus block per line.
  neuroskill/
    client.rs                   Label write over the daemon's HTTP API (bearer-gated).
    health.rs                   Periodic side-effect-free connection probe + status.
    eeg.rs                      Optional read-only intensity read-back + activity.sqlite
                                  path resolution (eeg-readback feature).
extension/                    First-party MV3 browser extension (claude.ai chat +
                                Claude Design, chatgpt.com) → the browser receiver.
                                providers.js centralizes the brittle DOM selectors,
                                keyed by host+path; loaded unpacked.
```

`engine/segment.rs` is the heart and is kept **pure** — feed it a `WorkEvent`
sequence and assert the blocks. Anything needing the wall clock or a file lives
in `orchestrator.rs`, which passes the result in.

### Surface adapters

Adding a surface = adding an adapter; nothing else changes. The only live one is
**Claude Code transcript watch** (zero-config): it watches
`~/.claude/projects/<encoded-cwd>/*.jsonl` with the `notify` crate and reads the
session's `cwd` for the project, new appended lines for activity, and prompt text
for confidence. The `.claude` root is auto-resolved: the native home
(`$HOME`/`%USERPROFILE%`), or — when Whence runs natively on Windows while Claude
Code runs inside WSL — a walk of the `\\wsl$\<distro>` homes for a
`.claude/projects` tree (pin it with the `claude_dir` setting if discovery picks
wrong). A network root like that gets a **polling** watcher, since OS file
notifications never cross the 9P bridge.

**Ollama liveness** (`ollama.rs`) is the second live surface — but a *status*
surface, not an attribution one. It polls Ollama's `/api/ps` and watches a model's
`expires_at` advance between polls (a bumped keep-alive = a request was just
served) to tell *inferring now* from *merely warm in memory*. When it sees fresh
inference it emits a low-confidence, **unattributed** `active` event — enough to
light the widget, but `project: None` so it can never originate or color a focus
block (the honest limit from §5.2: Ollama knows inference is happening, not *for
what*). It's **opt-in (default off)**: because that `active` is unattributed, with
the single status enum it can only show the widget as `active` with no project, so
you enable it via the `ollama` setting only if you want the bare liveness signal.

**Terminal cwd** (`terminal.rs`) is a *corroborator*, not an attribution source. A
one-line shell hook POSTs `{"cwd": "$PWD", "id": "$$"}` to a loopback `tiny_http`
listener (default `127.0.0.1:18451`, override via `terminal_listen_addr_override`)
on each directory change; the adapter resolves the cwd to a slug (alias map, then
the lossless basename) and emits a low-confidence `active` event. The optional `id`
(the shell PID) is the per-terminal source key, so two shells in one repo read as
two source rows; omitting it folds all terminals on a project into one. Low confidence is
load-bearing: the engine treats any event below `corroborator_confidence_cutoff`
as *reinforcing* — it can extend the current block (handy when you're working in
the terminal on the focused project with no AI activity) but **never** opens a
block from idle or originates a switch. A *present* block is fully protected from
it; the one bite it has is on a *running* block (one you may have stepped away
from), where a stray hint pointing at another repo drops the block — evidence
you've moved on — though even then it never opens the other project itself. It's
**opt-in (default off)** and needs the shell snippet — Whence never
edits shell rc files; enabling `terminal` alone does nothing until you add the
hook (the snippet is in Settings). The cwd-to-`WorkEvent` mapping is pure and
fixture-tested; only the socket is impure.

The **hooks receiver** (`hooks.rs`) complements transcript watch on the same
surface with *live status* the transcript can't cleanly infer — the difference
between Claude Code *running* and *awaiting your input*. It's a small loopback
`tiny_http` listener (default `127.0.0.1:18450`, override via
`hook_listen_addr_override`) that Claude Code's native `http` hooks POST to,
fire-and-forget: `Stop`/`Notification` → `awaiting_input`, `UserPromptSubmit` →
back to `active`. Installation is **opt-in** — `install_claude_hooks` (a button in
Settings) does a merge-preserving write of the hook config into
`.claude/settings.json` — the *same* `.claude` dir the transcript watcher
resolved, so on a Windows host with WSL Claude Code the hooks land in WSL's
settings, where Claude Code actually reads them — and `uninstall_claude_hooks`
round-trips it back out.
The event-to-`WorkEvent` mapping is pure and fixture-tested; only the socket is
impure.

**Browser LLM** (`browser.rs`) is the first **originating-capable** surface beyond
Claude Code: it self-attributes from the provider's *own* project identity, so it
can mint a project rather than merely corroborate one. A first-party MV3 browser
extension (`extension/`, loaded unpacked) reads only the project-scoped URL and the
provider's project id + name — a self-declared marker, never chat content or which
tab is focused — and POSTs them to a loopback `tiny_http` listener (default
`127.0.0.1:18452`, override via `browser_listen_addr_override`). The **daemon** owns
resolution: a normalized-URL fast path, then the provider project id (stable across
renames), else a fresh mint with `slug = slugify(name)`. Providers are selected by
**host + path-prefix**, so one host can carry more than one surface: `claude.ai`
serves both Claude chat and **Claude Design** (`/design`), each modeled as its own
provider id with its own keyspace and shown as a distinct source row (`claude web`
vs `claude design`) — converging with the filesystem project at the slug layer, not
the provider layer. Extension and daemon apply the same host+path rule. The
provider→slug map is a hand-editable TOML (`browser_mapping.toml`, surfaced in
Settings) written format-preservingly via `toml_edit`. A chat filed under no project resolves to
`project: None` and is dropped (ambient, not an error). Browser source rows are also
the widget's one *clickable* surface: clicking one raises its tab (the extension
polls a raise queue) and pins focus to that project via `focus_source` — a
you-acted switch through the normal engine path. It's **opt-in (default off)**
via the `browser` setting and needs the extension installed; the resolution and
event mapping are fixture-tested, only the socket and store I/O are impure. The
extension's DOM selectors are brittle by construction (provider markup churns) and
all live in one `providers.js` table to patch.

**Project slug** is the join key across the ecosystem — it must match WAID's
brief slug and Who Am I's naming. Resolved in priority order: a `project_aliases`
override keyed by transcript dir name, then the basename of the transcript's
first `cwd` line (the lossless source), then the dir-name trailing segment as a
provisional fallback. Filesystem basenames and browser-minted names alike pass
through one shared `slugify`, so the same project name reached from different
surfaces — a `~/Projects/whence` checkout and a "Whence" browser project —
converges onto a single node, no merge step.

### NeuroSkill labels — the write path

When the engine confirms a block on project P, Whence writes
`Whence:project=<slug>:start` / `:end` into NeuroSkill — source-namespaced so it
never collides with WAID's manual `waid:brief=<slug>:…` labels (downstream
prefers the manual label on conflict). **The label is the only thing written.**

The optional EEG read-back (behind the `eeg-readback` feature) is the one *read*,
and it is strictly read-only: it opens NeuroSkill's `activity.sqlite`
`mode=ro&immutable=1` and issues a single scoped `eeg_timeseries` query to power
the widget's intensity meter (mean `focus` over the last ~2 minutes, via
`get_focus_intensity`). The data dir is resolved the same WSL2-aware way as the
token, but against the daemon's *Local* AppData — explicit `neuroskill_data_dir`
setting → native local-data dir → WSL2 host discovery
(`/mnt/<drive>/Users/<user>/AppData/Local/NeuroSkill/activity.sqlite`). When the
feature is off or no store is found, the command returns nothing and the meter
hides — it never blocks focus tracking.

The daemon is reached over HTTP at `http://127.0.0.1:18444` by default. Token
resolution is **WSL2-aware**: an explicit `neuroskill_token_path` setting →
native `<config>/skill/daemon/auth.token` → WSL2 Windows-host discovery
(`/mnt/<drive>/Users/<user>/AppData/Roaming/skill/daemon/auth.token`), because
under WSL2 the daemon runs on the Windows host. Endpoint and token path are both
overridable in settings; the token is read at call time, so rotation needs no
restart.

Because labels are only written on block open/close — possibly minutes apart — a
separate **connection-health probe** (`health.rs`) keeps the widget honest about
whether the write path is live. On an interval it POSTs a benign no-op command to
the daemon (the bearer check runs before dispatch, so it exercises reachability
*and* auth without writing anything) and classifies the result: `connected`,
`unauthorized` (token rejected), `unreachable` (daemon down), or `disabled`
(label writing off). It reads settings each tick, so toggling the
`neuroskill_enabled` setting or editing the endpoint/token reflects live. The
status is pushed on a `whence://neuroskill` event (and readable via
`get_neuroskill_status`) and surfaces as a color-coded dot in the widget header —
diagnostic, never blocking: a down daemon just means the labels aren't written
this session.

---

## Develop

Prerequisites: Node 20+, pnpm, Rust toolchain, and the
[Tauri system dependencies](https://tauri.app/start/prerequisites/) for your OS.

```bash
pnpm install            # one-time setup
pnpm tauri dev          # run the widget (frontend + Rust backend)
pnpm tauri:wsl          # same, with the dmabuf renderer disabled for WSL2
pnpm tauri:dev2         # same, on port 1435 — run a second instance side by side
pnpm check              # svelte-check (frontend types)
```

```bash
cd src-tauri && cargo test                          # engine + adapter + store tests
cd src-tauri && cargo test --features eeg-readback  # include the SQLite read-back
```

The dev server runs on **port 1425** (WAID uses 1420, so both widgets can run at
once).

Pushing a `v*` tag builds Windows / macOS / Linux installers via the release
workflow (`.github/workflows/release.yml`).

### Plugins

- **`tauri-plugin-single-instance`** — registered first (a sensor must not run
  twice and double-write labels).
- **`tauri-plugin-window-state`** — the widget remembers its position.
- **`tauri-plugin-autostart`** — **opt-in only**, default OFF; toggled via the
  `autostart` setting, which reconciles the OS launch agent only when it changes.

---

## Status & scope

Shipped (v0/v1): the Tauri + Svelte shell, the Claude Code transcript watcher,
the segmentation engine — including the three-tier trust model, present-vs-
running attribution, and the per-project state registry — the JSONL timeline, the
NeuroSkill label write, and the widget (a project roster — one row per project with
rolled-up status + present/running + state timer, each expandable to its live
sources — plus an expanded today's-blocks timeline), the optional EEG intensity meter (read-only read-back, behind the
`eeg-readback` feature), the Claude Code hooks receiver for real-time
`awaiting_input` status (v1.5, opt-in), Ollama inference liveness (v1.5,
low-confidence status), the terminal cwd corroborator (v1.5, opt-in), the
NeuroSkill connection-health indicator (v1.5), and the browser LLM adapter —
claude.ai (chat + Claude Design) / chatgpt.com sessions via a first-party extension
(opt-in, originating-capable).
Planned: debounce calibration on real data (v1), then Who Am I inbox candidates
and WAID intention-vs-reality (v2). See [`whence-spec.md`](whence-spec.md) §13.

By design Whence does **not**: scrape OS window/app focus (banned by principle),
score or grade your focus (diagnostic only), touch the phone (desktop sensor
only), or auto-write to anyone's record (it proposes; humans promote). It runs
and stays entirely on-device.
