# Whence — Design Spec & Detailed Reference

> An ambient, on-screen focus widget and local sensor that infers **which
> project you're working on right now** from your AI-tool activity, displays it,
> and writes accurate attribution labels into NeuroSkill so the EEG data
> downstream knows *what* you were deep on — not just *how* deep.

This document is the detailed design reference: the "why" behind every decision,
plus the as-built behavior of each subsystem. The [`README.md`](README.md) is
the concise landing page; when it needs a detail, it points here. Where the
original spec and the shipped code diverged, this document now records what
shipped (e.g. the NeuroSkill write is HTTP, not WebSocket — see §8).

---

## 1. What Whence is

Whence is a **desktop sensor with an ambient widget**. It watches your work
surfaces (Claude Code, the browser, Ollama, the terminal), infers the project
you're focused on, shows that on screen, and produces attribution data the rest
of the ecosystem consumes.

Its place in the ecosystem is the key framing: **Whence is a producer, a peer
to NeuroSkill — not a consumer like WAID or Who Am I.**

* **NeuroSkill** measures focus *intensity* — how deep are you (EEG).
* **Whence** measures focus *attribution* — deep on *what*.

Two sensors, same downstream consumers. Fused, they answer a question neither
can alone: *"You were in deep focus for 90 minutes — on WAID's Figma
connector."* Today NeuroSkill only knows the intensity; attribution is either
missing or comes from WAID's manual session labels. Whence makes attribution
**automatic and continuous**.

### Why a new project instead of an edge between existing apps

Every integration we considered between the existing apps (What's Next? → Who
Am I, WAID → What's Next?) ran into the same wall: the desktop↔mobile,
no-backend device boundary. A new sensor that *natively produces* the
cross-cutting data sidesteps that wall for the desktop trio. (Honest limit:
Whence lives where you work — the desktop. Phone activity in What's Next? stays
out of scope and remains its own separate question.)

**Addendum (2026-07-31) — identity re-center.** The framing above is the
founding record and stays as written. In practice the widget became the product
and attribution became its feature: Whence is the local-LLM status widget
first, attribution sensor second. The "producer, not a consumer" line no longer
holds (the EEG read-back is a NeuroSkill read), and the sensor's first real
consumer is **WAID**, not Who Am I — briefs (intention) × timeline (attributed
reality) × EEG (depth) join in WAID's macro view. Contract:
[`docs/waid-ingestion-contract.md`](docs/waid-ingestion-contract.md); decision:
§14 · 9. The §2–§3 constraints are unaffected — they proved to be *why* the
status widget works.

---

## 2. The problem

You bounce between projects. You want to **know** when you're focused on one
versus another — not **tell** a tool every time you switch. The whole design
turns on one constraint:

> **Attribution without surveillance.**

There's a spectrum of how to get attribution, and only one point on it is
acceptable:

|Approach|Automatic?|Signal quality|Verdict|
|-|-|-|-|
|OS window / app focus scraping|Yes|Low (a backgrounded tab isn't focus)|**Banned** — the surveillance flavor the whole ecosystem rejects|
|Manual declaration ("I'm on WAID now")|No|High|Defeats the goal — you wanted to *know*, not *tell*|
|**Semantic work events** (prompt content, project context, session lifecycle from AI tools)|**Yes**|**High** (a prompt practically names the project)|**The target**|

Semantic work events read *your own work artifacts* rather than spying on
window chrome. They're higher-signal and they piggyback on actions you're
already taking. (Conceptual seed: the BCI / multi-agent cognitive-alignment
framing in arXiv:2606.13190 — cognitive state inferred from work signal, not
declared.)

---

## 3. Principles (non-negotiable)

1. **Semantic work events, never raw window/OS focus.** Whence reads what you
produce (prompts, cwd, session lifecycle), not which window is foregrounded.
2. **Piggyback — no standalone capture interrupts.** Whence never asks "what
are you doing?" Every signal rides an action you were already taking.
3. **Diagnostic, never evaluative.** The widget reports state; it never scolds
you for fragmenting or context-switching. The moment it judges, you stop
trusting it and the data dies.
4. **Local-first, no backend, no cloud, no accounts.** Same stance as WAID,
Who Am I, What's Next?. Everything runs and stays on-device. The one credential
in the system is *local integrity*, not identity: a per-install bearer token
that Whence's own loopback receivers require (§5.6), so another local process
can't forge attribution events.
5. **Read-scoped, bounded writes.** Mirror WAID's NeuroSkill discipline: read
only what's needed, and the *only* thing written into NeuroSkill is a session
label. No window titles, no terminal history, no file contents leave anything.
6. **Promote-gated downstream.** Whence *proposes* candidates to Who Am I and
surfaces signal to WAID; it never auto-writes to anyone's record. Consumers
keep their existing human-in-the-loop gates.
7. **Portable, inspectable data.** Whence's own focus timeline is a plain,
readable local store (JSONL — one block per line), inspectable and exportable.

---

## 4. Architecture

```
  SURFACE ADAPTERS            CORE                     OUTPUTS
  ───────────────            ────                     ───────
  Claude Code  ─┐                                  ┌─► Widget (roster + focus)
   (transcripts │            ┌──────────────┐       │
    + hooks)    │            │ Segmentation │ Focus ├─► NeuroSkill labels (HTTP write)
  Browser LLM  ─┼─ WorkEvent │   engine     │ blocks│
                │   stream  →│ (debounce)   │      ─┼─► Focus timeline store (JSONL)
  Terminal cwd ─┤            └──────────────┘       │
                │                                   └─► Consumer candidates
  Ollama       ─┘                                        (Who Am I inbox / WAID) — v2
```

* **Adapters** are per-surface and pure(-ish): they translate a surface's
native signal into a normalized `WorkEvent` and emit it on the core channel.
Adding a surface = adding an adapter; nothing else changes.
* **The core** ingests the `WorkEvent` stream, runs **segmentation** to decide
the *current focus* and detect real switches, and maintains the focus-block
timeline. `engine/segment.rs` is **pure and fixture-tested** — no I/O, no clock
reads; every time comes in via the event or an explicit `now`.
* **The orchestrator** (`orchestrator.rs`) is the impure seam: it wires
adapters → segmenter → outputs, owns the wall clock and the filesystem, and
attaches display-only **context strings** (§9) *downstream* of segmentation —
a tripwire test forbids any context reference under `engine/` or `neuroskill/`.
* **Outputs** are all driven off the same focus state: the widget, the
NeuroSkill label writes, the local timeline store, and (later) the consumer
candidates.

---

## 5. Surface adapters

### 5.1 Claude Code (primary — transcripts + hooks)

The richest, cleanest signal. Two complementary modes, both shipped:

**Transcript watch (zero-config baseline, always on).** Claude Code writes
per-project session transcripts under `~/.claude/projects/<encoded-cwd>/*.jsonl`.
Watching this tree (with the `notify` crate) gives, with no setup:

* **project** ← the session's `cwd` line (see §6 for the resolution ladder)
* **activity / timing** ← new lines appended to the active transcript
* **semantic content** ← prompt text (for confidence + later candidate drafting)

The `.claude` root is auto-resolved: the native home (`$HOME` /
`%USERPROFILE%`), or — when Whence runs natively on Windows while Claude Code
runs inside WSL — a walk of the `\\wsl$\<distro>` homes for a
`.claude/projects` tree. Pin it with the `claude_dir` setting if discovery
picks wrong. A network root like `\\wsl$` gets a **polling** watcher, since OS
file notifications never cross the 9P bridge.

**Hooks receiver (real-time status, opt-in install).** Claude Code's native
`http` hooks give **live status** the transcript can't cleanly infer — the
difference between Claude Code *running* and *awaiting your input*. A small
loopback `tiny_http` listener (default `127.0.0.1:18450`, override via
`hook_listen_addr_override`) receives fire-and-forget POSTs:
`Stop` / `Notification` → `awaiting_input`, `UserPromptSubmit` → back to
`active`. The listener is bearer-gated (§5.6); since `http` hooks can't set
headers, the token rides in the installed URL (`/hook/<token>`), so the
install/uninstall round-trip carries auth with zero extra steps.

Installation is **opt-in and explicit** (principle 2): `install_claude_hooks`
(a button in Settings) does a merge-preserving write of the hook config into
`.claude/settings.json` — the *same* `.claude` dir the transcript watcher
resolved, so on a Windows host with WSL Claude Code the hooks land in WSL's
settings, where Claude Code actually reads them. `uninstall_claude_hooks`
round-trips the config back out. Install prunes stale Whence entries under the
same base first (an old token, or a pre-auth bare `/hook`), so upgrading or
rotating the token is just clicking **Install hooks** again — rotation rewrites
installed hooks automatically. The event-to-`WorkEvent` mapping is pure and
fixture-tested; only the socket is impure.

### 5.2 Ollama (liveness / status only)

Ollama exposes a local REST API at `http://localhost:11434`. Whence polls
`/api/ps` and watches a model's `expires_at` advance between polls — a bumped
keep-alive means a request was just served — to tell *inferring now* from
*merely warm in memory*.

**Honest limitation:** Ollama tells you *local inference is happening*, not
*which project it's for*. When it sees fresh inference the adapter emits a
low-confidence, **unattributed** `active` event — enough to light the widget,
but `project: None`, so it can never originate or color a focus block. (The
temporal-correlation enrichment considered in the original spec was decided
against: liveness only — see §14.)

**Opt-in, default off** (`ollama_enabled`): because the event is unattributed,
all it can do is show the widget as `active` with no project, so you enable it
only if you want the bare liveness signal. Activity detection is pure and
fixture-tested; only the poll is impure.

### 5.3 Terminal cwd (corroborator, opt-in)

A one-line shell hook POSTs `{"cwd": "$PWD", "id": "$$"}` to a loopback
`tiny_http` listener (default `127.0.0.1:18451`, override via
`terminal_listen_addr_override`, bearer-gated — the snippet carries the
receiver token as an `Authorization` header) on each directory change. The
adapter resolves the cwd to a slug (§6) and emits a low-confidence `active`
event. The optional `id` (the shell PID) is the per-terminal source key, so two
shells in one repo read as two source rows; omitting it folds all terminals on
a project into one.

**Low confidence is load-bearing:** the engine treats any event below
`corroborator_confidence_cutoff` as *reinforcing* — it can extend the current
block (handy when you're working in the terminal on the focused project with no
AI activity) but **never** opens a block from idle or originates a switch. A
*present* block is fully protected from it; its one bite is on a *running*
block (§7), where a stray hint pointing at another repo drops the block —
evidence you've moved on — though even then it never opens the other project.

**Opt-in, default off** (`terminal_enabled`), and it needs the shell snippet —
Whence never edits shell rc files; enabling the setting alone does nothing
until you add the hook (the snippet is in Settings). The cwd-to-`WorkEvent`
mapping is pure and fixture-tested; only the socket is impure.

### 5.4 Claude Desktop chat tab (deferred)

Claude Desktop's chat tab has no clean local artifact or hook to read, so the
only capture options are fragile (accessibility-tree scraping) or manual.
**Deferred** — Claude Code + the browser are the clean signals; don't let the
hardest surface gate releases.

### 5.5 Browser LLM chat (originating-capable, opt-in)

The first **originating-capable** surface beyond Claude Code: it
self-attributes from the provider's *own* project identity, so it can mint a
project rather than merely corroborate one.

A first-party MV3 browser extension (`extension/`, shipped unlisted via the
Chrome Web Store with a load-unpacked zip as fallback — see
`docs/extension-distribution.md`) reads only
the project-scoped URL and the provider's project id + name — a self-declared
marker, never chat content or which tab is focused — and POSTs them to a
loopback `tiny_http` listener (default `127.0.0.1:18452`, override via
`browser_listen_addr_override`, bearer-gated — the token is pasted once into
the extension's options page, stored in `chrome.storage.local`, and sent as a
header on observations and the `/raise` poll).

**The daemon owns resolution**, in priority order: a normalized-URL fast path,
then the provider project id (stable across renames), else a fresh mint with
`slug = slugify(name)`. The provider→slug map is a hand-editable TOML
(`browser_mapping.toml`, path surfaced in Settings) written
format-preservingly via `toml_edit`, so backfills and mints never clobber your
hand-edits, comments, or ordering. A chat filed under no project resolves to
`project: None` and is dropped — ambient, not an error. Attributing these
ambient/root chats is planned future work.

Providers are selected by **host + path-prefix**, so one host can carry more
than one surface: `claude.ai` serves both Claude chat and **Claude Design**
(`/design`), each modeled as its own provider id with its own keyspace and
shown as a distinct source row (`claude web` vs `claude design`) — converging
with the filesystem project at the *slug* layer, not the provider layer.
Extension and daemon apply the same host+path rule.

Browser source rows are the widget's one *clickable* surface: clicking one
raises its tab (the extension polls a raise queue via `GET /raise`) and pins
focus to that project via the `focus_source` command — a you-acted switch
through the normal engine path, not a bypass.

**Opt-in, default off** (`browser_enabled`), and it needs the extension
installed. Resolution and event mapping are fixture-tested; only the socket and
store I/O are impure. The extension's DOM selectors are brittle by construction
(provider markup churns) and all live in one `providers.js` table, keyed by
host+path, to patch in one place.

### 5.6 Receiver auth (all loopback listeners)

All three loopback receivers — hooks `18450`, terminal `18451`, browser `18452`
(including `GET /raise`) — are **bearer-gated** by a per-install token
(`auth.rs`): minted at first launch, stored `0600` as
`<app_data_dir>/receiver.token`, shown and rotatable in Settings, applied live
via a shared handle so rotation needs no restart.

Carriers differ per receiver: the installed hook URL embeds the token
(`/hook/<token>` — `http` hooks can't set headers); the terminal snippet and
the extension send `Authorization: Bearer`. Unauthenticated requests get 401
and bump a visible denial counter — diagnostic, never evaluative. The request
check (`request_authorized`) is pure and fixture-tested; the sockets stay the
only impure part.

---

## 6. The `WorkEvent` model

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
  kind: "session_start" | "prompt" | "tool_use" | "awaiting_input"
      | "active" | "idle" | "session_end";
  confidence: number;    // 0..1 — cwd-derived ~1.0, weak corroborator ~0.4
  detail?: string | null;// optional semantic payload (prompt summary, etc.)
}
```

`source` is what the roster (§9) groups and counts on: `project` groups
sources, `source` distinguishes the concurrent ones within a group. Adapters
mint it stably — Claude Code from the transcript file id, the browser adapter
from its `provider_project_id` / tab identity, terminal from the shell PID.

### Project-slug resolution

`project` resolution turns a `cwd` / repo path into a stable **project slug** —
the join key across the whole ecosystem, so it must match WAID's
`waid:brief=<slug>` and Who Am I's project naming. The Claude Code adapter
(`adapters/claude_code.rs`) resolves it in priority order:

1. **Alias override** — `project_aliases` (settings), keyed by the transcript
**directory name** (the full encoded cwd, e.g. `-root-Projects-glue-mac`). The
user's explicit last word; wins over everything.
2. **Cached cwd result** — a per-directory memo so a session's cwd is read
once, not re-derived on every transcript append.
3. **`cwd` read from the transcript** (`slug_from_cwd`) — the basename of the
session's launch directory, taken from its *first* `cwd` line. The only
lossless source (the dir-name's `/`→`-` encoding is ambiguous — `glue-mac` →
`mac`, `one-domino-square` → `square`), and using the *launch* cwd means a
later `cd` into a subdir doesn't move attribution. Cached.
4. **Dir-name heuristic** (`slug_from_transcript_dir`) — the lossy
trailing-segment fallback, used only when no `cwd` line is readable (older
transcripts); *not* cached, so a later read can upgrade it.

The terminal corroborator resolves the **same** slug from a raw cwd — alias
override keyed by the cwd re-encoded to that dir-name form (`encode_dir_name`),
else the cwd basename — so a single alias map governs every surface. Because
the alias key is the full encoded path, it is **unique per absolute path** (two
repos each rooted at a folder named `app` get distinct keys), making
`project_aliases` the deterministic disambiguation layer.

Filesystem basenames and browser-minted names alike pass through one shared
`slugify`, so the same project reached from different surfaces — a
`~/Projects/whence` checkout and a "Whence" browser project — converges onto a
single node, no merge step.

Two wrinkles worth recording:

* A Claude Code **managed-worktree** path
(`<repo>/.claude/worktrees/<generated-name>`) collapses to `<repo>` at every
cwd→slug site — worktree sessions are work on the repo, not a throwaway
per-worktree project. (The context-string *root* stays the worktree itself, so
the branch line reflects the checkout actually worked in.)
* A transcript *synced from another machine* carries that machine's cwd — this
only affects historical files, since the adapter reads the changed (local)
file; pin such cases with an alias.

---

## 7. Segmentation engine (the hard part — this is where the design earns its name)

Capture is easy; **deciding when a switch actually happened** is the
intelligence. A 30-second glance at another repo is *not* a context switch. An
hour is. Get this wrong and the timeline reads "47 flickers today" instead of
"3 real blocks."

The engine holds **one answer at a time** — the project you're on now — and
changes it only when it's sure you've actually switched. Same project in → keep
the current block going and write nothing. A different project in → it must
prove itself before any label moves. Comparison is by *project*, not by file:
bouncing between two files in the same repo is a no-op. (This is the one spot
NeuroSkill's activity tracker differs — it re-records on every file change.)

### How much each signal is trusted

Every `WorkEvent` falls into one of three tiers by how much it may change the
current answer. The tier is read off `kind` + `confidence` — no model change;
§6 already separates `prompt` from `tool_use`.

* **You acted** — `prompt`, `session_start` at full confidence. You're actually
here. Strongest: can open a block, trigger and confirm a switch, and *protect*
the current project from being pulled away by weaker signals.
* **Claude worked on its own** — `tool_use` and other autonomous transcript
growth. High confidence in *which* project the work is on, low confidence in
whether you're watching. Keeps the current block alive (and can open one from
idle), but is slow to trigger a switch and never protects the current project.
* **Weak hint** — anything below `corroborator_confidence_cutoff` (terminal
cwd). Only nudges: reinforces whatever's already current; can't open, switch,
or protect.

Status events (`active` / `awaiting_input` / `idle`) sit outside all three —
they drive the widget immediately and never move a block (see *Status vs
focus* below).

### Rules

* **Same project** → extend the current block (push `end`, bump `eventCount`,
fold `confidence` into `meanConfidence`); clear any candidate. Nothing is
written.
* **Different project P′** → P′ becomes a *candidate*; how fast it accumulates
depends on its tier:
  * *You acted* → switches immediately (explicit intent needs no debounce) and
marks the block *present*.
  * *Claude worked* → builds a candidate slowly, and only once your interactive
signal on the current project has gone stale (no `prompt` within the
attention-recency window). A background task on P′ can't yank you off a project
you're actively prompting on — only off one you've stopped touching.
  * *Weak hint* → adds no candidate weight.
* **Confirm the switch** when the candidate clears the **debounce threshold** —
sustained evidence over a window (`switch_min_seconds`, ~60–120s), not a single
event. On confirm: close the current block (write
`Whence:project=<current>:end`, append the block to the timeline) and open the
new one (`Whence:project=<new>:start`).
* **Idle**: no *you-acted* or *Claude-worked* event for `idle_timeout_seconds`
(~5–10 min) ends the block (status → idle; no project attributed to the gap).
Weak hints don't reset it.
* **`session_end`** → close the current block.

### Present vs running (honest attribution while work runs unattended)

Track when you last *acted* (last `prompt`) separately from when the project
last saw *any* activity:

* a block is **present** while you've prompted within the attention-recency
window — you're here;
* it flips to **running** once only autonomous `tool_use` has arrived since
your last prompt — Claude is still going, but you may have stepped away.

Either way the block stays open and **keeps accruing time** — autonomous work
is still real work on that project. What changes is how hard it holds:

* a **present** block makes any competing autonomous signal earn the full
debounce before it can switch away, and is fully protected from weak hints;
* a **running** block yields — a `prompt` in another project switches away
immediately, and even a single weak hint pointing at another repo **drops** the
block (evidence you've moved on), though a weak hint never opens the other
project itself.

The widget shows which one it is (`· present` vs `· running` after the project
name), so the attribution never *claims* your attention when all it has is
Claude's. This is the answer to "a background task on WAID while your eyes are
on a browser": Whence holds WAID (the work is real, and it can't see your eyes
without the banned window signal), labels it *running*, and drops it the moment
any real signal points elsewhere.

### Calibration knobs

All four live in settings, calibrated against your actual switching rhythm:

* `switch_min_seconds` (~60–120s) — sustained evidence before a switch confirms.
* `idle_timeout_seconds` (~5–10 min) — the quiet gap that ends a block.
* `corroborator_confidence_cutoff` (~0.6) — at/above it a signal can drive
switches; below it only corroborates.
* `attention_recency_seconds` (~120) — the present-vs-running boundary.

### Status vs focus — a deliberate split

* **Focus switches** go through the debounce engine (stability matters; flicker
is the enemy).
* **Status changes** (`active` ↔ `awaiting_input` ↔ `idle`) **surface
immediately** to the widget. Status is not a focus switch — it's low
flicker-risk and high responsiveness-value (you *want* the widget to light up
the instant Claude Code is waiting on you). Status never moves the focus block
boundary.

### The state registry (what the roster reads — additive, alongside the one focus block)

The engine still holds **exactly one focus block** — a human is on one project
at a time, and that single answer is what drives the NeuroSkill label write
(§8). Nothing above changes.

What the widget's **roster** (§9) needs is a second, read-only projection the
engine maintains beside the focus block: a **per-project state registry**. It
attributes no focus and writes no label — it just records, for every project
that currently has a live or recently-live source, enough to render a row:

* **sources** — the live `source`s (§6) grouped under the project's slug, each
carrying its own surface, label, `last_activity`, and status.
* **attention status** — `active` / `awaiting_input` / `idle`, rolled up from
the project's sources (most-attention-demanding wins: any `awaiting_input` →
AWAITING YOU; else any `active` → ACTIVE; else IDLE). Driven by the immediate
status path above, *per project* — **including projects that are not the
current focus**.
* **lifecycle** — the *present* / *running* distinction, computed per project
rather than only for the open block: *present* if you prompted that project
within the attention-recency window, *running* if only autonomous `tool_use`
has arrived since, and *neither* (dormant — shown blank) when a project is in
the registry only because it was recently seen.
* **entered_at** — when the project entered its current attention status, so
the roster can show a live "time in state" timer.

Two independent axes, both shown per row: **lifecycle** (present / running / —)
as the qualifier after the name, **attention status** (ACTIVE / AWAITING YOU /
IDLE) as the pill.

Eviction keeps the roster short: a project drops out once it has been `idle`
past a roster-retention window (longer than the idle timeout — idle rows
linger, greyed, before disappearing), so the roster is a current list, not a
growing log. The timeline store stays the durable history; the registry is live
state only.

---

## 8. NeuroSkill enrichment (requirement #1 — the write path)

This is what makes Whence a *producer*. When the focus engine confirms a block
on project P, Whence writes session labels into NeuroSkill so its EEG epochs
get attributed to P — automatically, continuously, no manual session-start.

* **Mechanism (as built): the daemon's HTTP API.** The original spec assumed
WAID's WebSocket contract (`ws://127.0.0.1:8375`); the shipped daemon exposes
**HTTP** — `neuroskill/client.rs` POSTs the `label` command to
`http://127.0.0.1:18444` (bearer-token gated), lifted from WAID's
`neuroskill/client.rs`. Endpoint overridable via `neuroskill_endpoint`.
* **The only write is the label**: `Whence:project=<slug>:start` / `:end` on
block open/close. Same discipline as WAID — read nothing Whence doesn't need.

### Token resolution (WSL2-aware)

Under WSL2 the daemon runs on the *Windows host*, so the Linux config dir has
no token and an unauthenticated call 401s; the daemon is still reachable at
`127.0.0.1:18444` thanks to `.wslconfig` mirrored networking. Resolution order
(`client::resolve_token_path`):

1. explicit `neuroskill_token_path` setting;
2. native `<config>/skill/daemon/auth.token`, if it exists;
3. WSL2 Windows-host discovery — a walk of
`/mnt/<drive>/Users/<user>/AppData/Roaming/skill/daemon/auth.token`.

The token is read at call time, so rotation or late discovery needs no restart.

### Coordination with WAID's manual labels (decided)

Both WAID and Whence write labels into the same `labels` table. **Resolution:
namespace by source, manual wins.** `waid:` = explicit/manual, `Whence:` =
inferred/auto; when an EEG epoch falls under both, downstream consumers prefer
the manual one (you explicitly said so; Whence only guessed). The
Whence-as-sole-labeler alternative was rejected as coupling the two repos.

### Connection health (keeps the widget honest)

Labels are only written on block open/close — possibly minutes apart — so a
separate probe (`neuroskill/health.rs`) checks the write path on an interval:
it POSTs a benign no-op command (the bearer check runs before dispatch, so it
exercises reachability *and* auth without writing anything) and classifies the
result — `connected`, `unauthorized` (token rejected), `unreachable` (daemon
down), or `disabled` (label writing off). It reads settings each tick, so
toggling `neuroskill_enabled` or editing the endpoint/token reflects live. The
status is pushed on a `whence://neuroskill` event (readable via
`get_neuroskill_status`) and surfaces as a color-coded dot in the widget header
— diagnostic, never blocking: a down daemon just means the labels aren't
written this session.

### Optional read-back (powers the widget's intensity meter)

Whence can read NeuroSkill's EEG back — double-gated: the `eeg-readback` cargo
feature (a *default* feature; `--no-default-features` drops the SQLite
dependency from the binary) and the runtime `eegReadbackEnabled` setting
(**opt-in, default off** — it's the one place Whence touches a biosignal, so it
stays explicit; toggled in Settings → NeuroSkill, where the data-dir override
reveals under it). Strictly read-only: it opens `activity.sqlite` with
`mode=ro&immutable=1` and
issues a single scoped `eeg_timeseries` query (mean `focus` over the last ~2
minutes, via `get_focus_intensity`). A read-scope guard test in
`neuroskill/eeg.rs` pins the discipline. The data dir is resolved the same
WSL2-aware way as the token, but against the daemon's *Local* AppData: explicit
`neuroskill_data_dir` → native local-data dir →
`/mnt/<drive>/Users/<user>/AppData/Local/NeuroSkill/activity.sqlite`. When
either gate is off or no store is found, the command returns nothing and the
meter hides — it never blocks focus tracking.

---

## 9. The widget (requirement #3 — the on-screen surface)

A small, **always-on-top, borderless, transparent** Tauri window — the same
window techniques WAID already uses for its custom chrome.

The widget is a **project roster**: one row per project that currently has a
live or recently-live source (from the state registry, §7), with the **current
focus project** pinned at the top. A human is on one project at a time, so
exactly one row is the focus; the others are shown so you can see what else is
running or waiting on you without it being what you're attributed to.

**Header:** the `BrandMark`, the NeuroSkill connection dot (§8), settings, and
a collapse control for the whole roster.

**Each project row shows:**

* **Project** — name + the shared brand color/glyph for that project.
* **Lifecycle qualifier** — *present* / *running* / — , per the state registry,
as a subtle suffix after the name, so the widget never implies your attention
when only Claude's is on the work.
* **Attention status** — ACTIVE / AWAITING YOU / IDLE as a colored pill + dot
(green / amber / grey), *per project*.
* **State timer** — time in the current attention status.
* **Expand chevron** — reveals the project's live **sources**: each with its
surface label (`terminal 1`, `chatgpt web`, `claude design`), its own status,
and its own timer, so a project with three concurrent sessions reads as three
lines, not one blurred row. Browser source rows are clickable (raise the tab +
pin focus, §5.5).

**Context string** — beside the focused project, a display-only
`git branch · last commit` line (design: [`docs/context-strings.md`](docs/context-strings.md)).
Branch is a pure HEAD-file parse (worktree `gitdir:` redirects followed);
subject is a best-effort `git log -1` spawn with a 2 s timeout and silent
degrade. Roots come from a slug→root side table the transcript watcher fills
from the `cwd` it already reads. The orchestrator attaches it downstream of
segmentation via wrapper types (`WidgetSnapshot` / `TimelineRecord`,
serde-flattened) — the NeuroSkill label stays byte-identical, and the git-tier
string is also stamped onto closing timeline blocks. Two content-derived
*moment* sources are opt-in, default off, and **display-only** (never stamped,
A2 raw-capture posture, pinned by test): **`HookPrompt`**
(`context_hook_prompts`) — your prompt snippet / session summary via the hooks
receiver — and **`BrowserTitle`** (`context_browser_titles`) — the
conversation's title via the extension, double-gated (the `/raise` poll carries
`capture_titles`, so the extension strips titles at the source when off).
Toggle: `context_strings` (default on); TTL: `context_ttl_seconds` (file-only
knob).

**Ordering:** by attention priority — ACTIVE first, then AWAITING YOU, then
IDLE (idle rows greyed, aged out per roster retention). Strictly a priority
sort, never a ranking or a count of how fragmented your day was (principle 3).

**Daily history** — the expanded view shows today's blocks
(`BlockTimeline`: "3 blocks today: WAID 2h10m · whatsnext 40m"-style), fed by
`get_today_blocks`. Diagnostic only — no "you switched 9 times 😬."

**Intensity meter** — the optional EEG read-back (§8) on the focus row, when
enabled and connected.

**Behaviors:** draggable, remembers position, quiet "unattributed / idle" state
rather than going blank. Settings open in their own popup window
(`open_settings`, `routes/settings`). Appearance knobs: `widget_opacity`
(constant, user-set, floored at 0.3 — never adaptive), `dark_mode`,
`always_on_top` (default on), `always_present` (visible across
workspaces — macOS/Linux only). Reuses the "Tidewater · Refined" token system
and `BrandMark` from WAID so it reads as part of the family.

---

## 10. Downstream consumers (v2 — the payoff)

Whence's value compounds when its blocks flow outward. Both consumers keep
their existing human gates; Whence only *proposes*.

* **Who Am I (highest-value pipe).** Who Am I's core friction is honest recall
of what you did, and it already has the exact intake shape Whence needs: an
ingest **inbox with a promote gate**. Whence drafts candidates like *"Spent 3h
on waid — wiring the Figma connector"* (project from the block, semantic detail
from prompt content) into that inbox. Nothing reaches the record without your
explicit promote. Whence sees *all* desktop work, not just timed task sessions.
*Deferred (2026-07-31): the timeline's first real consumer is WAID (§14 · 9);
this pipe stays open but unscheduled.*
* **WAID (intention vs. reality).** WAID knows your *active* brief; Whence
knows where the day *actually* went. Surface the delta — *"waid is your active
brief, but today went to whatsnext"* — strictly diagnostic. Design spec:
[`docs/intention-vs-reality.md`](docs/intention-vs-reality.md).

---

## 11. Tech stack (as built)

Matches the desktop trio so it shares identity and reuses known patterns:

* **Shell + backend:** Tauri 2 (Rust).
* **Frontend:** SvelteKit, Svelte 5 (runes), SPA mode (`adapter-static`,
`ssr=false`), Tailwind v4. WAID's "Tidewater · Refined" tokens + `BrandMark`.
* **Rust crates:** `notify` (transcript watching; polling mode on network
roots), `tiny_http` (the three loopback receivers — synchronous, each on its
own thread; no async server framework), `reqwest` (Ollama poll + the NeuroSkill
HTTP label write), `toml_edit` (format-preserving browser mapping store),
`getrandom` (receiver-token minting), `tokio` (runtime; `process` for the
context-string git spawn — no `net` feature), `serde` / `serde_json`, `chrono`,
and optionally `rusqlite` (bundled, behind the default-on `eeg-readback` feature).
* **Window:** always-on-top + transparent + borderless (lifted from WAID's
transparent-window approach), plus a system tray as the only un-hide path.
* **Data:** JSONL for the focus timeline — one block per line (see the
`engine/timeline.rs` module doc for why JSONL beats SQLite for this workload).
No server, no accounts.

---

## 12. Project structure (as built)

```
whence/
├── src/                          # SvelteKit frontend (the widget)
│   ├── routes/                   # +layout, widget view, settings popup route
│   └── lib/
│       ├── components/           # FocusBadge, StatusDot, ProjectRow, SourceRow,
│       │                         #   BlockTimer, BlockTimeline, IntensityMeter,
│       │                         #   NeuroskillStatusDot, SettingsPanel, Toggle, BrandMark
│       ├── stores/               # focus / neuroskill / appearance (runes)
│       ├── tauri.ts              # the only place naming backend commands + events
│       ├── tauri.mock.ts         # mock backend (VITE_WHENCE_MOCK=1; screenshots)
│       └── types.ts              # TS mirror of the Rust serde models
├── src-tauri/src/
│   ├── lib.rs                    # plugin + command + window registration
│   ├── commands.rs               # the Tauri command surface
│   ├── orchestrator.rs           # adapters → segmenter → outputs (the impure seam)
│   ├── auth.rs                   # receiver bearer token (§5.6)
│   ├── context.rs                # context strings (§9)
│   ├── settings.rs               # JSON settings file in the app data dir
│   ├── tray.rs                   # system tray (restore + quit)
│   ├── adapters/                 # mod (WorkEvent + slugify), claude_code, hooks,
│   │                             #   ollama, terminal, browser, browser_map
│   ├── engine/
│   │   ├── segment.rs            # debounce / switch / blocks — PURE, fixture-tested
│   │   └── timeline.rs           # JSONL store, one block per line
│   └── neuroskill/
│       ├── client.rs             # label write over HTTP (§8)
│       ├── health.rs             # connection-health probe (§8)
│       └── eeg.rs                # optional read-only read-back (§8)
├── extension/                    # first-party MV3 extension (§5.5)
└── whence-spec.md                # this document
```

Keep `engine/segment.rs` **pure and fixture-tested** — feed it a `WorkEvent`
sequence, assert the blocks. It's the heart of the system and the easiest thing
to get subtly wrong. Anything needing the wall clock or a file lives in
`orchestrator.rs`, which passes the result in.

---

## 13. Build phases

* **v0 — skeleton (done).** Tauri + Svelte shell; Claude Code transcript
watcher; widget shows current project. Proved the signal is real.
* **v1 — the product (done).** Segmentation engine (debounce, blocks, idle);
the three-tier trust model + present/running (§7); immediate status path;
per-project state registry + roster widget; NeuroSkill label write; JSONL
timeline. Remaining: debounce calibration on real data.
* **v1.5 — fidelity (done).** Claude Code hooks receiver (real-time
`awaiting_input`); Ollama liveness; terminal cwd corroborator; EEG read-back
intensity meter; settings UI; receiver auth (§5.6); NeuroSkill
connection-health dot; browser LLM adapter + extension (§5.5); context strings
(§9); settings popup window + appearance controls.
* **v2 — payoff pipes.** WAID timeline ingestion + intention-vs-reality (§10;
contract in [`docs/waid-ingestion-contract.md`](docs/waid-ingestion-contract.md));
surface coverage expansion — including IDE adapters (Cursor, Codex, IntelliJ,
VS Code), each following the §4 rule: one new file under `adapters/`, nothing
else changes. Who Am I inbox candidates deferred.
* **v2 (optional) — richer candidates.** Thread an aggregated semantic `detail`
summary into `FocusBlock` so WAID's ingest can draft *what* you did, not just
*how long*. The block carries time-attribution today (`project`, `start`/`end`,
`eventCount`, `meanConfidence`), but `WorkEvent.detail` is dropped at block
close. Touches `engine/segment.rs` (carry detail through the open block),
`engine/timeline.rs` (schema), `src/lib/types.ts` (sync). Ingestion contract:
[`docs/waid-ingestion-contract.md`](docs/waid-ingestion-contract.md) — WAID
reads `<app_data_dir>/timeline.jsonl` (and `corrections.jsonl` once A3 lands —
resolve latest-wins, skip dropped) and dedupes on `(project, start)`.

---

## 14. Decisions log

Originally the open-decisions list; now the record of how each fork resolved.

1. **Claude Desktop chat-tab capture** — **deferred** (still open; §5.4).
2. **v1 surface allowlist** — resolved by shipping: transcripts first, hooks +
Ollama + terminal in v1.5, browser after.
3. **Awaiting-input gating** — **resolved:** status surfaces immediately, focus
switches go through the engine (§7).
4. **Label precedence with WAID** — **resolved:** namespace by source +
manual-wins; two parallel writers, no coupling (§8).
5. **Ollama attribution** — **resolved:** liveness only, unattributed; it never
colors a block (§5.2).
6. **EEG read-back timing** — **resolved:** v1.5 polish, behind the
`eeg-readback` feature (§8); later re-resolved as a runtime opt-in — the
feature compiles in by default and the `eegReadbackEnabled` setting (default
off) gates the read, so enabling the meter no longer needs a rebuild (§8).
7. **Widget shape** — **resolved:** one focus block + a roster, additive; the
registry is a read-only projection (§7, §9).
8. **Daily-history placement** — **resolved:** the expanded view's
today's-blocks timeline (§9), not the row-expand action (which opens sources).
9. **Product identity & first consumer (2026-07-31)** — **resolved:**
widget-first — Whence is the local-LLM status widget; attribution demotes from
thesis to feature (§1 addendum). The timeline's first real consumer is WAID
([`docs/waid-ingestion-contract.md`](docs/waid-ingestion-contract.md)); Who Am
I inbox candidates deferred. The §2–§3 non-surveillance constraints are
retained verbatim — they are load-bearing for the widget identity, not just
the sensor's. Narrative:
[`docs/case-study-identity-pivot.md`](docs/case-study-identity-pivot.md).

---

## 15. Out of scope

* **Mobile / What's Next? phone activity** — desktop sensor only; the phone box
stays sealed (its own separate question).
* **OS-level window/app/usage tracking** — banned by principle 1.
* **Cloud, sync, accounts, multi-user.**
* **Evaluative scoring / "focus grades" / nudges to switch less** — banned by
principle 3. Diagnostic only.
* **Auto-writing to Who Am I's record or WAID's briefs** — Whence proposes;
humans promote.
