# Whence

An ambient focus widget and **local attribution sensor**. Whence infers which
project you're working on from your LLM-tool activity, shows it in a small
(optionally) always-on-top widget. 

Multiple projects? Multiple prompts? Status at a glance and a timeline of where your actual focus lies. 

### NeuroSkill

Whence writes attribution labels into NeuroSkill so the EEG data downstream knows *what* you were deep on — not just *how* deep.


NeuroSkill measures focus *intensity*; Whence measures focus *attribution*. It
is a producer and a peer to NeuroSkill, not a consumer. The full design lives
in [`whence-spec.md`](whence-spec.md); this README is the short version.

### Core constraints
_Attribution without surveillance._

- Reads the work artifacts you already produce — prompts, cwd, session
  lifecycle. Never which window is foregrounded.
- Never interrupts to ask "what are you doing?"
- Diagnostic, never evaluative — no "you switched 9 times".
- Local-first: no backend, no cloud, no accounts. The only credential is a
  per-install token its own loopback receivers require, so other local
  processes can't forge attribution.

## How it works

```
adapters/  →  engine/segment.rs  →  outputs (widget · NeuroSkill labels · timeline)
```

Adapters normalize each surface's signal into `WorkEvent`s. The **segmentation
engine** debounces them into focus *blocks* — a 30-second glance at another
repo is not a context switch. Each confirmed block drives the widget, a
NeuroSkill label, and the local JSONL timeline. Three rules carry most of the
design:

- **Trust tiers.** `Present` vs `Running`
  - *You acting* (a prompt, a session start) switches focus immediately and marks the block `present`. 
  - *Claude working on its own* only
    builds a switch candidate once you've gone quiet and the block reads
    `running`. 
  - *Weak hints* below `corroborator_confidence_cutoff` (terminal
    cwd) only reinforce the current block and a `running` block drops
    on a stray hint pointing elsewhere. 
  - The widget shows `present` vs `running` so it never implies your attention when only an LLM is on the
    work.
- **Status ≠ focus.** `active` / `awaiting_input` / `idle` are shown for your convenience. It moves a block boundary.
- **One focus, many rows.** The widget is a roster of every live project —
  each row expandable to its concurrent sources (sessions, tabs, terminals) —
  but only the single focused project is attributed, so persisted blocks never
  overlap.

The focused row also carries a **context string** 
- Git status `git branch · last commit` OR opt-in `prompt/title snippet`
- display-only, never labeled) — see [`docs/context-strings.md`](docs/context-strings.md).

## Surfaces

Adding a surface = adding an adapter (`src-tauri/src/adapters/`); nothing else
changes. Details for each in spec §5.

| Surface | Signal | Role | Default |
|---|---|---|---|
| Claude Code transcripts | `~/.claude/projects/**/*.jsonl` watch | attribution (primary) | on, zero-config |
| Claude Code hooks | native `http` hooks → `127.0.0.1:18450` | live `awaiting_input` status | opt-in install |
| Browser LLM | MV3 extension (`extension/`) → `127.0.0.1:18452` | originating attribution — claude.ai chat + Design, chatgpt.com | opt-in |
| Terminal cwd | shell hook POST → `127.0.0.1:18451` | weak corroborator | opt-in |
| Ollama | `/api/ps` poll | unattributed liveness status | opt-in |

Worth knowing:

- Transcript watch auto-resolves the `.claude` root natively or across the
  `\\wsl$` boundary (pin with the `claude_dir` setting); network roots get a
  polling watcher.
- Hook install/uninstall is a Settings button doing a merge-preserving
  round-trip of `.claude/settings.json`; the receiver token rides in the hook
  URL, and rotation rewrites installed hooks.
- The extension reads only project-scoped URLs and the provider's project
  id/name — never chat content or tab focus. The provider→slug map is
  hand-editable TOML. Browser source rows are clickable: raise the tab and pin
  focus.
- All three loopback receivers are **bearer-gated** by a per-install token
  (`auth.rs`) — minted at first launch, shown/rotatable in Settings, rejected
  requests counted visibly.

**Project slug** is the join key across the ecosystem (WAID briefs, Who Am I
naming). Resolution: `project_aliases` override → basename of the transcript's
first `cwd` line (lossless) → dir-name fallback. Every surface shares one
`slugify`, so a `~/Projects/whence` checkout and a "Whence" browser project
converge with no merge step. Claude Code managed-worktree paths collapse to
their repo. 

## NeuroSkill

On block open/close Whence writes `Whence:project=<slug>:start|end` over the
daemon's HTTP API (`127.0.0.1:18444`) — namespaced so WAID's manual `waid:`
labels win on conflict. **The label is the only write.**

Token and data-dir resolution are WSL2-aware (explicit setting → native config
dir → Windows-host discovery under `/mnt/<drive>/…`), and the token is read at
call time, so rotation needs no restart. A periodic health probe surfaces the
write path as a header dot — `connected` / `unauthorized` / `unreachable` /
`disabled` — diagnostic, never blocking.

The optional EEG read-back (`--features eeg-readback`) is the one *read*:
strictly `mode=ro&immutable=1` against NeuroSkill's `activity.sqlite`, powering
the widget's intensity meter. 

## Develop

Prerequisites: Node 20+, pnpm, Rust toolchain, and the
[Tauri system dependencies](https://tauri.app/start/prerequisites/) for your OS.

```bash
pnpm install            # one-time setup
pnpm tauri dev          # run the widget (frontend + Rust backend)
pnpm tauri:wsl          # same, with the dmabuf renderer disabled for WSL2
pnpm tauri:dev2         # second instance side by side, on port 1435
pnpm check              # svelte-check (frontend types)
pnpm dev:mock           # frontend only, mock backend data (no Rust)
pnpm screenshots        # headless screenshot capture (uses mock mode)
```

```bash
cd src-tauri && cargo test                          # engine + adapter + store tests
cd src-tauri && cargo test --features eeg-readback  # include the SQLite read-back
```

The dev server runs on **port 1425** (WAID uses 1420, so both widgets can run
at once). Releases: `pnpm release <version>` tags and pushes; CI builds
Windows/macOS/Linux installers into a draft release — see
[`RELEASE.md`](RELEASE.md).

### Layout

```
src/                    SvelteKit widget (Svelte 5 runes, SPA, Tailwind v4)
  lib/tauri.ts            the only place naming backend commands + events
  lib/tauri.mock.ts       mock backend (VITE_WHENCE_MOCK=1; drives screenshots)
  lib/types.ts            TS mirror of the Rust serde models — keep in sync
  lib/stores/ · lib/components/ · routes/   (settings is its own popup window)
src-tauri/src/
  orchestrator.rs         wires adapters → engine → outputs (the impure seam)
  auth.rs                 receiver bearer token: mint, pure check, denial counter
  context.rs              context strings (branch · commit + opt-in moments)
  commands.rs             the Tauri command surface (see src/lib/tauri.ts)
  settings.rs · tray.rs · lib.rs
  adapters/               one file per surface + browser_map.rs (TOML store)
  engine/segment.rs       debounce / switch / blocks — PURE, fixture-tested
  engine/timeline.rs      JSONL local store, one block per line
  neuroskill/             client.rs (label write) · health.rs · eeg.rs
extension/              MV3 extension; brittle DOM selectors live in providers.js
```

Keep `engine/segment.rs` **pure** — no I/O, no clock reads; anything impure
lives in `orchestrator.rs` and passes results in.

Plugins: `tauri-plugin-single-instance` (registered first — a sensor must not
run twice and double-write labels), `window-state` (position memory), and
`autostart` (opt-in, default OFF).

## Status & scope

v0–v1.5 shipped: transcript watcher, segmentation engine (trust tiers,
present/running, state registry), roster widget with settings window and
appearance controls, JSONL timeline, NeuroSkill labels + health dot, hooks
receiver, Ollama liveness, terminal corroborator, browser extension, receiver
auth, context strings, EEG intensity meter. Planned: debounce calibration on
real data, then Who Am I inbox candidates and WAID intention-vs-reality (spec
§13).

By design Whence does **not**: scrape OS window/app focus, score or grade your
focus, touch the phone, or auto-write to anyone's record (it proposes; humans
promote). It runs and stays entirely on-device.

## License

[GPL-3.0-or-later](LICENSE).
