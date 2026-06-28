# Whence — Project Spec

> An ambient, on-screen focus widget and local sensor that infers \*\*which
> project you're working on right now\*\* from your AI-tool activity, displays it,
> and writes accurate attribution labels into NeuroSkill so the EEG data
> downstream knows \*what\* you were deep on — not just \*how\* deep.

This document is the starting spec for a new repo. It consolidates the design
decisions made so far and flags the forks that are still open.

\---

## 1\. What Whence is

Whence is a **desktop sensor with an ambient widget**. It watches your work
surfaces (Claude Code, Ollama, later others), infers the project you're focused
on, shows that on screen, and produces attribution data the rest of the
ecosystem consumes.

Its place in the ecosystem is the key framing: **Whence is a producer, a peer
to NeuroSkill — not a consumer like WAID or Who Am I.**

* **NeuroSkill** measures focus *intensity* — how deep are you (EEG).
* **Whence** measures focus *attribution* — deep on *what*.

Two sensors, same downstream consumers. Fused, they answer a question neither can
alone: *"You were in deep focus for 90 minutes — on WAID's Figma connector."*
Today NeuroSkill only knows the intensity; attribution is either missing or comes
from WAID's manual session labels. Whence makes attribution **automatic and
continuous**.

### Why a new project instead of an edge between existing apps

Every integration we considered between the existing apps (What's Next? → Who Am
I, WAID → What's Next?) ran into the same wall: the desktop↔mobile, no-backend
device boundary. A new sensor that *natively produces* the cross-cutting data
sidesteps that wall for the desktop trio. (Honest limit: Whence lives where
you work — the desktop. Phone activity in What's Next? stays out of scope and
remains its own separate question.)

\---

## 2\. The problem

You bounce between projects. You want to **know** when you're focused on one
versus another — not **tell** a tool every time you switch. The whole design
turns on one constraint:

> \*\*Attribution without surveillance.\*\*

There's a spectrum of how to get attribution, and only one point on it is
acceptable:

|Approach|Automatic?|Signal quality|Verdict|
|-|-|-|-|
|OS window / app focus scraping|Yes|Low (a backgrounded tab isn't focus)|**Banned** — the surveillance flavor the whole ecosystem rejects|
|Manual declaration ("I'm on WAID now")|No|High|Defeats the goal — you wanted to *know*, not *tell*|
|**Semantic work events** (prompt content, project context, session lifecycle from AI tools)|**Yes**|**High** (a prompt practically names the project)|**The target**|

Semantic work events read *your own work artifacts* rather than spying on window
chrome. They're higher-signal and they piggyback on actions you're already
taking. (Conceptual seed: the BCI / multi-agent cognitive-alignment framing in
arXiv:2606.13190 — cognitive state inferred from work signal, not declared.)

\---

## 3\. Principles (non-negotiable)

1. **Semantic work events, never raw window/OS focus.** Whence reads what you
produce (prompts, cwd, session lifecycle), not which window is foregrounded.
2. **Piggyback — no standalone capture interrupts.** Whence never asks "what
are you doing?" Every signal rides an action you were already taking.
3. **Diagnostic, never evaluative.** The widget reports state; it never scolds
you for fragmenting or context-switching. The moment it judges, you stop
trusting it and the data dies.
4. **Local-first, no backend, no cloud, no auth.** Same stance as WAID, Who Am I,
What's Next?. Everything runs and stays on-device.
5. **Read-scoped, bounded writes.** Mirror WAID's NeuroSkill discipline: read
only what's needed, and the *only* thing written into NeuroSkill is a session
label. No window titles, no terminal history, no file contents leave anything.
6. **Promote-gated downstream.** Whence *proposes* candidates to Who Am I and
surfaces signal to WAID; it never auto-writes to anyone's record. Consumers
keep their existing human-in-the-loop gates.
7. **Portable, inspectable data.** Whence's own focus timeline is a plain,
readable local store (SQLite or JSONL), inspectable and exportable.

\---

## 4\. Architecture

```
  SURFACE ADAPTERS            CORE                     OUTPUTS
  ───────────────            ────                     ───────
  Claude Code  ─┐                                  ┌─► Widget (current focus)
                │            ┌──────────────┐       │
  Ollama       ─┼─ WorkEvent │ Segmentation │ Focus ├─► NeuroSkill labels (WS write)
                │   stream  →│   engine     │ blocks│
  Terminal cwd ─┤            │ (debounce)   │      ─┼─► Focus timeline store (local)
                │            └──────────────┘       │
  Claude Desktop┘  (deferred)                       └─► Consumer candidates
                                                         (Who Am I inbox / WAID) — phased
```

* **Adapters** are per-surface and pure: they translate a surface's native
signal into a normalized `WorkEvent` and emit it. Adding a surface = adding an
adapter; nothing else changes.
* **The core** ingests the `WorkEvent` stream, runs **segmentation** to decide
the *current focus* and detect real switches, and maintains the focus-block
timeline.
* **Outputs** are all driven off the same focus state: the widget, the NeuroSkill
label writes, the local timeline store, and (later) the consumer candidates.

\---

## 5\. Surface adapters

### 5.1 Claude Code (primary, v1)

The richest, cleanest signal. Two complementary modes — use both:

* **Transcript watch (zero-config baseline).** Claude Code writes per-project
session transcripts under `\~/.claude/projects/<encoded-cwd>/\*.jsonl`. Watching
this tree (with the `notify` crate) gives, with no setup:

  * **project** ← the session's `cwd` (the directory name encodes it)
  * **activity / timing** ← new lines appended to the active transcript
  * **semantic content** ← prompt text (optional; for confidence + later
candidate drafting)
* **Hooks (real-time status enrichment).** Claude Code supports lifecycle hooks
(`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`,
`SessionEnd`) configured in `.claude/settings.json`. A hook can POST a small
JSON event to Whence's localhost endpoint. This gives **live status** that
transcript-watching can't cleanly infer — in particular the difference between
*running* and *awaiting your input*. Hook installation is a one-time, opt-in
setup step (Whence offers to write the hook config; never silently).

Recommendation: **ship transcript-watch first** (works immediately, no setup),
**layer hooks** for status fidelity.

### 5.2 Ollama (liveness / status, v1.5)

Ollama exposes a local REST API at `http://localhost:11434`. Poll `/api/ps` for
running/loaded models and whether inference is active.

**Honest limitation:** Ollama tells you *local inference is happening*, not
*which project it's for*. Treat it as a **liveness/status signal**, not an
attribution source. Optional enrichment: attribute Ollama activity by **temporal
correlation** with the currently-focused project (if you're running a local model
while Whence's focus is WAID, tag it WAID) — but flag this as **lower
confidence** in the `WorkEvent`, and never let it *originate* a focus switch on
its own.

### 5.3 Terminal cwd (optional, v1.5)

A shell hook (or watching a shell's cwd) can emit a low-confidence project hint.
Useful as a tiebreaker / corroborator, not a primary signal. Optional.

### 5.4 Claude Desktop chat tab (**deferred — open decision, see §14**)

Claude Desktop's chat tab has no clean local artifact or hook to read, so the
only capture options are fragile (accessibility-tree scraping) or manual. **Spec
recommendation: defer for v1.** Claude Code + Ollama are the clean signals; don't
let the hardest surface gate the first release.

### 5.5 Browser LLM chat (new surface adapter)

First-class browser AI-chat capture (claude.ai, chatgpt.com, …) as an originating, rootless surface adapter — full design in [`docs/browser-llm-adapter.md`](docs/browser-llm-adapter.md).

\---

## 6\. The `WorkEvent` model

Every adapter emits the same normalized shape:

```ts
interface WorkEvent {
  ts: string;            // ISO timestamp
  surface: "claude-code" | "ollama" | "terminal" | "browser" | "claude-desktop";
  source: string;        // stable per-instance source id (one Claude Code session,
                         // one browser tab, …) — distinct concurrent sources on the
                         // same surface/project get distinct ids; the roster keys on it
  label?: string | null; // optional human label ("terminal 1", "chatgpt web") for
                         // the roster; derived from surface + index if absent
  project: string | null;// resolved project slug (null = unattributed activity)
  kind: "session\_start" | "prompt" | "tool\_use" | "awaiting\_input"
      | "active" | "idle" | "session\_end";
  confidence: number;    // 0..1 — cwd-derived \~1.0, temporal-correlation \~0.4
  detail?: string | null;// optional semantic payload (prompt summary, etc.)
}
```

`source` (new) is what the roster (§9) groups and counts on: `project` groups
sources, `source` distinguishes the concurrent ones within a group. Adapters mint it
stably — Claude Code from the transcript file id, the browser adapter from its
`provider\_project\_id` / tab identity, terminal from the cwd.

`project` resolution turns a `cwd` / repo path into a stable **project slug** — the
join key across the whole ecosystem, so it must match WAID's `waid:brief=<slug>` and
Who Am I's project naming. The Claude Code adapter (`adapters/claude\_code.rs`)
resolves it in priority order:

1. **Alias override** — `project\_aliases` (settings), keyed by the transcript
**directory name** (the full encoded cwd, e.g. `-root-Projects-glue-mac`). The user's
explicit last word; wins over everything.
2. **Cached cwd result** — a per-directory memo so a session's cwd is read once, not
re-derived on every transcript append.
3. **`cwd` read from the transcript** (`slug\_from\_cwd`) — the basename of the
session's launch directory, taken from its *first* `cwd` line. The only lossless
source (the dir-name's `/`→`-` encoding is ambiguous), and using the *launch* cwd
means a later `cd` into a subdir doesn't move attribution. Cached.
4. **Dir-name heuristic** (`slug\_from\_transcript\_dir`) — the lossy trailing-segment
fallback, used only when no `cwd` line is readable (older transcripts); *not* cached,
so a later read can upgrade it.

The terminal corroborator (`adapters/terminal.rs`) resolves the **same** slug from a
raw cwd — alias override keyed by the cwd re-encoded to that dir-name form
(`encode\_dir\_name`), else the cwd basename — so a single alias map governs every
surface.

Because the alias key is the full encoded path, it is **unique per absolute path**
(two repos each rooted at a folder named `app` get distinct keys). That makes
`project\_aliases` the deterministic naming / disambiguation layer: same path in →
same slug out, with an alias as the guaranteed override when the default basename
collides or reads wrong.

\---

## 7\. Segmentation engine (the hard part — this is where the design earns its name)

Capture is easy; **deciding when a switch actually happened** is the intelligence.
A 30-second glance at another repo is *not* a context switch. An hour is. Get this
wrong and the timeline reads "47 flickers today" instead of "3 real blocks."

The engine holds **one answer at a time** — the project you're on now — and changes
it only when it's sure you've actually switched. Same project in → keep the current
block going and write nothing. A different project in → it must prove itself before
any label moves. Comparison is by *project*, not by file: bouncing between two
files in the same repo is a no-op. (This is the one spot NeuroSkill's activity
tracker differs — it re-records on every file change.)

### How much each signal is trusted

Every `WorkEvent` falls into one of three tiers by how much it may change the
current answer. The tier is read off `kind` + `confidence` — no model change; §6
already separates `prompt` from `tool\_use`.

* **You acted** — `prompt`, `session\_start` at full confidence. You're actually
here. Strongest: can open a block, trigger and confirm a switch, and *protect* the
current project from being pulled away by weaker signals.
* **Claude worked on its own** — `tool\_use` and other autonomous transcript growth.
High confidence in *which* project the work is on, low confidence in whether you're
watching. Keeps the current block alive (and can open one from idle), but is slow
to trigger a switch and never protects the current project.
* **Weak hint** — anything below the corroborator confidence cutoff (terminal cwd,
Ollama temporal correlation). Only nudges: reinforces whatever's already current;
can't open, switch, or protect.

Status events (`active` / `awaiting\_input` / `idle`) sit outside all three — they
drive the widget immediately and never move a block (see *Status vs focus* below).

### Rules

* **Same project** → extend the current block (push `end`, bump `eventCount`, fold
`confidence` into `meanConfidence`); clear any candidate. Nothing is written.
* **Different project P′** → P′ becomes a *candidate*; how fast it accumulates
depends on its tier:
  * *You acted* → builds at full weight.
  * *Claude worked* → builds slowly, and only once your interactive signal on the
current project has gone stale (no `prompt` within the attention-recency window).
A background task on P′ can't yank you off a project you're actively prompting on —
only off one you've stopped touching.
  * *Weak hint* → adds no candidate weight.
* **Confirm the switch** when the candidate clears the **debounce threshold** —
sustained evidence over a window, not a single event (≥ `SWITCH\_MIN\_SECONDS`, or
≥ N events; start \~60–120s). On confirm: close the current block (write
`Whence:project=<current>:end`, append the block to the timeline) and open the new
one (`Whence:project=<new>:start`).
* **Idle**: no *you-acted* or *Claude-worked* event for `IDLE\_TIMEOUT` (5–10 min)
ends the block (status → idle; no project attributed to the gap). Weak hints don't
reset it.
* **`session\_end`** → close the current block.

### Present vs running (honest attribution while work runs unattended)

Track when you last *acted* (last `prompt`) separately from when the project last
saw *any* activity:

* a block is **present** while you've prompted within the attention-recency window —
you're here;
* it flips to **running** once only autonomous `tool\_use` has arrived since your
last prompt — Claude is still going, but you may have stepped away.

Either way the block stays open and **keeps accruing time** — autonomous work is
still real work on that project. What changes is how hard it holds:

* a **present** block makes any competing signal earn the full debounce before it
can switch away;
* a **running** block yields easily — a `prompt` in another project, or even a
single weak hint pointing at another repo, switches away immediately, no waiting.

The widget shows which one it is (`waid · active` vs `waid · running`), so the
attribution never *claims* your attention when all it has is Claude's. This is the
answer to "a background task on WAID while your eyes are on a browser": Whence holds
WAID (the work is real, and it can't see your eyes without the banned window
signal), labels it *running*, and drops it the moment any real signal points
elsewhere.

### Calibration

`SWITCH\_MIN\_SECONDS` (\~60–120s) and `IDLE\_TIMEOUT` (\~5–10 min) already exist; the
attention-aware behavior adds two more knobs: a **corroborator confidence cutoff**
(\~0.6 — at or above it a signal can drive switches; below it only corroborates) and
an **attention-recency window** (\~2 min — the present-vs-running boundary). All live
in config and are surfaced read-only for v1 so the debounce can be calibrated
against your actual switching rhythm.

### Status vs focus — a deliberate split (resolves one of the open forks)

The old open question was whether `awaiting\_input` transitions route through the
cognitive engine or surface immediately. **Recommendation: split them.**

* **Focus switches** go through the debounce engine (stability matters; flicker is
the enemy).
* **Status changes** (`active` ↔ `awaiting\_input` ↔ `idle`) **surface
immediately** to the widget. Status is not a focus switch — it's low flicker-risk
and high responsiveness-value (you *want* the widget to light up the instant
Claude Code is waiting on you). Status never moves the focus block boundary.

### The state registry (what the roster reads — additive, alongside the one focus block)

The engine still holds **exactly one focus block** — a human is on one project at a
time, and that single answer is what drives the NeuroSkill label write (§8). Nothing
above changes.

What the widget's **roster** (§9) needs is a second, read-only projection the engine
maintains beside the focus block: a **per-project state registry**. It attributes no
focus and writes no label — it just records, for every project that currently has a
live or recently-live source, enough to render a row:

* **sources** — the live `source`s (§6) grouped under the project's slug, each
carrying its own surface, label, `last\_activity`, and status.
* **attention status** — `active` / `awaiting\_input` / `idle`, rolled up from the
project's sources (most-attention-demanding wins: any `awaiting\_input` → AWAITING
YOU; else any `active` → ACTIVE; else IDLE). Driven by the immediate status path
above, *per project* — **including projects that are not the current focus**.
* **lifecycle** — the *present* / *running* distinction from above, computed per
project rather than only for the open block: *present* if you prompted that project
within the attention-recency window, *running* if only autonomous `tool\_use` has
arrived since, and *neither* (dormant — shown blank) when a project is in the registry
only because it was recently seen but has had no prompt and no live work.
* **entered\_at** — when the project entered its current attention status, so the
roster can show a live "time in state" timer.

Two independent axes, both shown per row: **lifecycle** (present / running / —) as the
qualifier after the name, **attention status** (ACTIVE / AWAITING YOU / IDLE) as the
pill. These are the §7 notions exactly — the only change is that they are tracked for
*every* project in the registry, not just the one open focus block.

Eviction keeps the roster short: a project drops out once it has been `idle` past a
`ROSTER\_RETENTION` window (longer than `IDLE\_TIMEOUT` — idle rows linger, greyed,
before disappearing), so the roster is a current list, not a growing log. The timeline
store (the §7 blocks) stays the durable history; the registry is live state only.

\---

## 8\. NeuroSkill enrichment (requirement #1 — the write path)

This is what makes Whence a *producer*. When the focus engine confirms a block
on project P, Whence writes session labels into NeuroSkill so its EEG epochs
get attributed to P — automatically, continuously, no manual session-start.

* **Mechanism: reuse WAID's proven NeuroSkill contract.** WAID already writes
`waid:brief=<slug>:start` / `:end` labels over NeuroSkill's local WebSocket
(`ws://127.0.0.1:8375`, a hand-rolled `label` command — no SDK). Whence does
the same on focus-block open/close, namespaced as **`Whence:project=<slug>:start`
/ `:end`**.
* **The only write is the label.** Same discipline as WAID: read nothing
Whence doesn't need; the WebSocket label is the sole write.
* **Verify the contract at build time.** The WS command shape, port, and the
`labels` / `eeg\_timeseries` schema should be confirmed against NeuroSkill's
actual implementation (same verification WAID did). Lift WAID's `neuroskill/ws.rs`
approach as the reference.

### Coordination with WAID's manual labels (**new design decision — see §14**)

Both WAID and Whence now write labels into the same `labels` table. They must
not stomp each other, and downstream readers need a precedence rule. Proposed:

* **Namespace by source** (`waid:` = explicit/manual, `Whence:` = inferred/auto).
* **Manual wins on conflict.** When an EEG epoch falls under both a manual WAID
label and an inferred Whence label, downstream consumers prefer the manual
one (you explicitly said so; Whence only guessed).
* Alternative worth weighing: make **Whence the single labeler** and have WAID's
manual "Start session" defer to / override Whence rather than writing its own
parallel labels. Cleaner long-term, but couples the two repos. Decision in §14.

### Optional read-back (powers the widget's intensity display)

Whence *may* also read NeuroSkill's EEG back (read-only, `mode=ro\&immutable=1`,
exactly as WAID does) to show live focus intensity on the widget alongside
attribution. This closes the loop visually: *deep (EEG) on WAID (attribution)*.
Optional for v1; the write path is the requirement, the read-back is the polish.

\---

## 9\. The widget (requirement #3 — the on-screen surface)

A small, **always-on-top, borderless, transparent** Tauri window — the same window
techniques WAID already uses for its custom chrome.

The widget is a **project roster**: one row per project that currently has a live or
recently-live source (from the state registry, §7), with the **current focus
project** distinguished at the top. A human is on one project at a time, so exactly
one row is the focus; the others are shown so you can see what else is running or
waiting on you without it being what you're attributed to.

**Header:** the `BrandMark`, a **capture-liveness indicator** (is the sensor receiving
events / is the NeuroSkill WS up — the signal glyph in the mockup), settings, and a
collapse control for the whole roster.

**Each project row shows:**

* **Project** — name + the shared brand color/glyph for that project.
* **Lifecycle qualifier** — *running* / *present* / — , per the state registry (§7),
as a subtle suffix after the name (`whence · running`, `waid · present`) so the widget
never implies your attention when only Claude's is on the work.
* **Attention status** — ACTIVE / AWAITING YOU / IDLE as a colored pill + dot (green /
amber / grey), driven by the immediate status path (§7), *per project*.
* **State timer** — time in the current attention status (how long ACTIVE, how long
AWAITING YOU, how long IDLE).
* **Expand chevron** — reveals the project's live **sources**.

**Expanded row** (per project): its live sources from the registry — each with its
surface label (`terminal 1`, `terminal 2`, `chatgpt web`), its own status, and its own
timer — so a project with three concurrent sessions reads as three rows under one
project, not one blurred line.

**Ordering:** by attention priority — ACTIVE first, then AWAITING YOU, then IDLE (idle
rows greyed and de-emphasized, aged out per `ROSTER\_RETENTION`). Strictly a priority
sort, never a ranking or a count of how fragmented your day was (principle 3).

*(Optional)* **EEG intensity** — a small focus meter from the NeuroSkill read-back on
the focus row, when connected.

**Daily history** — the earlier "3 blocks today: WAID 2h10m · whatsnext 40m" timeline
summary still has a home, but it is **no longer the expand action** (expand now opens a
project's sources). **Open decision (§14):** a separate history view / tab, or a
second expand affordance. Either way it stays diagnostic-only — no "you switched 9
times 😬."

**Behaviors:** draggable, remembers position, click-through-when-idle optional,
a quiet "unattributed / idle" state rather than going blank. Reuse the
"Tidewater · Refined" token system and `BrandMark` from WAID so it reads as part
of the family.

\---

## 10\. Downstream consumers (phased — the payoff)

Whence's value compounds when its blocks flow outward. Both consumers keep
their existing human gates; Whence only *proposes*.

* **Who Am I (highest-value pipe).** Who Am I's core friction is honest recall of
what you did, and it already has the exact intake shape Whence needs: an
ingest **inbox with a promote gate**. Whence drafts candidates like
*"Spent 3h on waid — wiring the Figma connector"* (project from the block,
semantic detail from prompt content) into that inbox. Nothing reaches the record
without your explicit promote. This is the better version of the earlier
"feed Who Am I" instinct: Whence sees *all* desktop work, not just timed task
sessions.
* **WAID (intention vs. reality).** WAID knows your *active* brief; Whence knows
where the day *actually* went. Surface the delta — *"waid is your active brief,
but today went to whatsnext"* — strictly diagnostic. Design spec:
[`docs/intention-vs-reality.md`](docs/intention-vs-reality.md).

Both are **post-v1**. v1 is sensor + widget + NeuroSkill labels.

\---

## 11\. Tech stack

Matches the desktop trio so it shares identity and you reuse known patterns:

* **Shell + backend:** Tauri 2 (Rust).
* **Frontend:** SvelteKit, Svelte 5 (runes), SPA mode (`adapter-static`,
`ssr=false`), Tailwind v4. Reuse WAID's "Tidewater · Refined" tokens + `BrandMark`.
* **Rust crates:** `notify` (transcript watching), `reqwest` (Ollama API + the
localhost hook endpoint, or a tiny `axum`/`tiny\_http` listener for hooks),
`tokio` + a hand-rolled WS frame writer (NeuroSkill label, à la WAID's `ws.rs`),
`rusqlite` (local timeline store; bundled, and read-only NeuroSkill read-back if
enabled), `serde` / `serde\_json`, `chrono`.
* **Window:** always-on-top + transparent + borderless (Tauri window config; lift
WAID's transparent-window + resize-handle approach).
* **Data:** local SQLite (or JSONL) for the focus timeline. No server, no auth.

\---

## 12\. Proposed project structure

```
Whence/
├── src/                          # SvelteKit frontend (the widget)
│   ├── routes/                   # +layout, compact widget view, expanded timeline
│   └── lib/
│       ├── components/           # FocusBadge, StatusDot, BlockTimeline, IntensityMeter
│       ├── stores/               # focus state (runes), settings
│       ├── tauri.ts              # the only place naming backend commands
│       └── types.ts              # WorkEvent / FocusBlock / ProjectIdentity (mirror Rust)
├── src-tauri/
│   └── src/
│       ├── lib.rs                # plugin + command + window registration
│       ├── commands.rs           # focus state queries, settings, hook-install
│       ├── adapters/
│       │   ├── mod.rs            # WorkEvent model + adapter trait
│       │   ├── claude\_code.rs    # transcript watch + hook receiver
│       │   ├── ollama.rs         # /api/ps poll (liveness)
│       │   └── terminal.rs       # optional cwd hint
│       ├── engine/
│       │   ├── segment.rs        # debounce / switch confirmation / blocks (pure, fixture-tested)
│       │   └── timeline.rs       # local store read/write
│       └── neuroskill/
│           ├── ws.rs             # label write (Whence:project=<slug>:start|end)
│           └── eeg.rs            # optional read-only intensity read-back
└── CLAUDE.md                     # working guidance for Claude Code
```

Keep `engine/segment.rs` **pure and fixture-tested** — feed it a `WorkEvent`
sequence, assert the blocks. It's the heart of the system and the easiest thing to
get subtly wrong.

\---

## 13\. Build phases

* **v0 — skeleton + see it work.** Tauri + Svelte shell; Claude Code transcript
watcher; widget shows current project (raw, no debounce yet). Proves the signal
is real.
* **v1 — the product.** Segmentation engine (debounce, blocks, idle); immediate
status path; per-project state registry + roster widget (§7, §9); **Claude Code
hooks** for real-time `awaiting\_input` (pulled forward from v1.5 — AWAITING YOU is
core to the roster, and §5.1 notes transcript-watch alone can't cleanly infer it);
NeuroSkill label write; local timeline store. **This is the shippable sensor.**
* **v1.5 — fidelity.** Ollama liveness; optional terminal cwd; optional EEG
read-back / intensity meter.
* **v2 — payoff pipes.** Who Am I inbox candidates; WAID intention-vs-reality.
* **v2 (optional) — richer candidates.** Thread an aggregated semantic `detail`
summary into `FocusBlock` so a Who Am I connector ingesting `timeline.jsonl` can
draft *what* you did, not just *how long*. The block carries time-attribution
today (`project`, `start`/`end`, `eventCount`, `meanConfidence`), but
`WorkEvent.detail` (the prompt summary) is dropped at block close. Touches
`engine/segment.rs` (carry detail through the open block), `engine/timeline.rs`
(schema), `src/lib/types.ts` (sync). Ingestion contract: Who Am I reads
`<app_data_dir>/timeline.jsonl` directly and dedupes on `(project, start)`.

\---

## 14\. Open decisions (carry these into kickoff)

1. **Claude Desktop chat-tab capture** — accessibility scrape / manual / **defer**.
*Spec leans defer for v1.*
2. **v1 surface allowlist** — *Spec leans:* Claude Code (transcript) only for v1;
add hooks + Ollama in v1.5. Confirm.
3. **Awaiting-input gating** — *Resolved in §7:* status surfaces immediately,
focus switches go through the engine. Confirm you agree.
4. **NEW — label precedence with WAID** — namespace + manual-wins (two parallel
writers) **or** Whence-as-sole-labeler (WAID defers). Cleaner-but-coupled vs.
simpler-but-redundant.
5. **NEW — Ollama attribution** — liveness only, or temporal-correlation
enrichment (low-confidence)? Affects whether Ollama can ever color a block.
6. **NEW — EEG read-back in v1?** — is the widget's intensity meter v1 polish or
v1.5? (Write path is the requirement either way.)
7. **RESOLVED — widget shape** — **one focus block + a roster**, additive. A human is
on one project at a time, so the single focus block (→ NeuroSkill label) is unchanged;
the roster is a separate read-only projection (the state registry, §7) over every
project with a live/recent source. present/running and attention status are computed
*per project* for the roster, not only for the open block.
8. **NEW — daily-history placement** — the "3 blocks today" summary is no longer the
expand action (expand opens a project's sources). Separate history view/tab, or a
second expand affordance? *Leaning: separate view.*

\---

## 15\. Out of scope

* **Mobile / What's Next? phone activity** — desktop sensor only; the phone box
stays sealed (its own separate question).
* **OS-level window/app/usage tracking** — banned by principle 1.
* **Cloud, sync, accounts, multi-user.**
* **Evaluative scoring / "focus grades" / nudges to switch less** — banned by
principle 3. Diagnostic only.
* **Auto-writing to Who Am I's record or WAID's briefs** — Whence proposes;
humans promote.

```

