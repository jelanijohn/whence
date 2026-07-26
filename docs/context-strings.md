# Context strings

**Status:** Implemented (v1, git source) — 2026-07-26. See §12 for where the implementation deviates from the placeholder names below.
**Scope:** New display-only feature. No engine changes, no label changes, no changes to any adapter's capture surface.

## 1. Purpose

Whence attributes *which* project you're in; it says nothing about *what you were doing there*. A context string is a short, human-readable line shown beside the attribution — in the live widget and on past timeline blocks — to jog memory:

> **whence** · feat/context-strings — add HEAD parser fixtures

Display only. It never leaves Whence.

## 2. Invariants (load-bearing)

1. **Never in labels.** The NeuroSkill label remains exactly `Whence:project=<slug>`. No context text, no source tag, nothing appended, ever.
2. **Never an attribution input.** The engine (`engine/segment.rs`) neither receives nor emits context. Context is attached downstream by the orchestrator, after segmentation. Engine signatures unchanged; purity untouched.
3. **Silent degradation.** Any failure to resolve a context string yields `None` — never a surfaced error, never a delayed snapshot or event.
4. **v1 sources are user-authored only.** Branch names and commit subjects are text the user typed. Content-derived sources (prompts, conversation titles) are out of scope (§10), each parked behind its own explicit gate.

A tripwire test pins invariants 1–2 (§9).

## 3. Model

New module `src-tauri/src/context.rs`:

```rust
pub struct ContextString {
    pub text: String,          // sanitized, ≤ 120 chars
    pub source: ContextSource,
    pub observed_at: DateTime<Utc>,
}

pub enum ContextSource {
    Git,
    // future: HookPrompt, BrowserTitle, CdpTitle, WaidBrief (§10)
}
```

- `FocusSnapshot` gains `context: Option<ContextString>` (serde default; mirrored field-for-field in `types.ts` per existing convention).
- The timeline block record gains an optional `context: { text, source }`, stamped at block close (§5). Additive JSONL change: readers must tolerate absence; existing lines are unaffected.

## 4. v1 source: git

Resolved only for the focused project, and only when it has a `root` (browser-minted rootless projects yield `None`).

**Branch — required, pure file read.**
Resolve `<root>/.git`:

- If `.git` is a *file* (worktree/submodule case): parse the `gitdir: <path>` line and follow it; relative paths resolve against `root`.
- Read `HEAD`:
  - `ref: refs/heads/<branch>` → branch is `<branch>` (strip the prefix; keep nested slashes, e.g. `feat/tray-icon`).
  - Bare SHA (detached) → branch is `@<first 7 chars>`.
- Missing or unparseable → the whole string is `None`.

No spawn and no libgit2 dependency for this half. (Packed refs are irrelevant here — the branch *name* comes straight from the HEAD file.)

**Commit subject — best-effort, spawned.**
`git -C <root> log -1 --format=%s`, hard timeout ~2 s. Spawn failure, timeout, or non-zero exit silently drops the subject; branch alone still shows. Rationale: hand-parsing loose/packed objects isn't worth it, a `git` binary is a reasonable soft dependency, and cold `\\wsl.localhost` paths may be slow — degrade to branch-only rather than stall.

**Formatting.**
`"<branch> · <subject>"`, or just `"<branch>"` when no subject resolved. Sanitize: strip control characters, collapse whitespace, trim; truncate to 120 chars on a char boundary with `…`.

**Refresh & caching.**
Cache per root: `{ ContextString, fetched_at }`. Resolve:

- on focus switch to a rooted project — asynchronously: the snapshot emits immediately without context, and a follow-up emit attaches it once resolution completes;
- lazily, when the cached value is older than `context_ttl_seconds` (default 60) at the next snapshot emit for that project.

Never resolve for non-focused projects; never block the event path.

## 5. Persistence

At block close, the orchestrator stamps the block with the last resolved context for that project, if any. This is what makes past timeline rows recallable — the stated purpose of the feature. Git-tier strings are user-authored and low-sensitivity, so persistence ships default-on with the feature. Future content-derived sources must re-litigate persistence individually (§10).

No retro-stamping: blocks closed before the feature existed, or while it was toggled off, simply lack the field.

## 6. Display

**Widget (live focus row).** A secondary line under the project name: muted foreground token, single line, CSS ellipsis, full text available via `title`. When `context` is `None` the line is absent — no placeholder, no reserved height.

**Timeline (expanded rows).** Show the block's stored context in the same muted treatment. Absent field → nothing rendered.

Diagnostic tone throughout. No source glyphs or icons in v1 — there is only one source.

## 7. Settings

One toggle: **"Context strings"** — default **on**. Off means no resolution, no snapshot field, no block stamping.

`context_ttl_seconds` is a settings-file-only knob (no UI), default 60.

Writes follow the merge-preserving `toml_edit` path and existing key-naming conventions in `settings.rs`.

## 8. Touchpoints

1. `src-tauri/src/context.rs` — new: types, HEAD parser, subject spawn, per-root cache.
2. `orchestrator.rs` — attach context on snapshot emit; stamp on block close; TTL check.
3. `settings.rs` — `context_strings: bool` (default `true`), `context_ttl_seconds: u64` (default `60`).
4. `commands.rs` — expose the toggle per the existing settings-command pattern.
5. `types.ts` — mirror `ContextString` / `ContextSource` and both new optional fields.
6. Widget focus-row component and expanded timeline-row component — the secondary line.
7. `SettingsPanel.svelte` — the toggle.

No capability changes; no `lib.rs` changes anticipated. Implementer discipline: read the actual snapshot-emit and block-close call sites before wiring item 2, and match existing settings/TS naming rather than the placeholder names above.

## 9. Testing

- **HEAD parser fixtures:** normal branch, nested branch (`feat/x/y`), detached SHA, gitfile/worktree redirection, missing file, garbage content.
- **Sanitization/truncation:** control chars, overlong subjects, multibyte truncation at a char boundary.
- **Serde tolerance:** pre-existing snapshot/block JSON without the new fields still deserializes.
- **Tripwire (gstack A5 pattern):** a static test asserting no import or reference of `ContextString` / `context::` anywhere under `engine/` or `neuroskill/`; plus a regression assert that the label constructor's output for a context-bearing project is byte-identical to today (`Whence:project=<slug>`). Failure message names §2 of this doc.
- Put the subject spawn behind a small seam (trait or `cfg`) so unit tests don't require a git binary.

## 10. Future sources (out of scope — each behind its own gate)

Priority ladder once multiple sources exist: freshest per-moment string wins (hook / browser / CDP over git), per-project fallback last (WAID one-liner, provider project name). Every future source is opt-in, off by default, and truncated at the same cap.

- **Hook prompt snippet / transcript summary** (`HookPrompt`) — **implemented 2026-07-26**: `UserPromptSubmit` carries the prompt text (sanitized snippet), and `SessionStart` reads the session summary from the head of the transcript the hook's `transcript_path` points at. Content-derived → opt-in (`contextHookPrompts`, default off), gated live at the hooks receiver so the text is dropped at the capture boundary when off. Persistence decision: **display-only** — hook-derived strings are never stamped onto timeline blocks (the A2 raw-capture posture); the block stamp stays git-tier. A moment is cleared when its block closes; git is the fallback. See §12.
- **Browser conversation title** (`BrowserTitle`) — **implemented 2026-07-26**: one `document.title` read (provider suffix stripped, generic untitled forms discarded), sent as `conversation_title`. The extension's capture-surface table was amended first (extension/README.md, "Conversation titles") — the documented scope change this bullet required. Opt-in (`contextBrowserTitles`, default off) and double-gated: the daemon advertises the setting on the `/raise` poll (`capture_titles`) so the extension strips titles at the source when off, and the receiver drops them again in depth. Display-only like every content-derived source; recorded only for *attributed* conversations, never an attribution input. See §12.
- **Desktop CDP target title** (`CdpTitle`): available from `/json/list` without attaching, but outside `desktop-cloud-chat-attribution.md`'s "URL + network timing metadata only" read scope. Requires an explicit scope amendment there first.
- **WAID brief one-liner** (`WaidBrief`): reuse the brief-peek frontmatter join as the per-project fallback; resolve the display overlap with the existing peek affordance at that point.
- Parked niceties: dirty-worktree marker (`*`), per-source glyphs.

## 11. Open items

1. Is spawning `git` from the Windows-side daemon against `\\wsl.localhost` roots acceptably fast in practice? Spec assumes yes given the 2 s timeout and silent degrade; if not, v1 ships branch-only and the subject moves to §10.
2. `context_ttl_seconds` default (60) — calibrate by feel once live.

## 12. Implementation notes (as-built deltas)

The §8 discipline ("match the actual code, not the placeholder names") produced these deviations:

- **Wrapper types, not engine fields.** `FocusSnapshot`/`FocusBlock` live under `engine/`, which the §9 tripwire keeps context-free — so the orchestrator wraps them: `WidgetSnapshot { #[serde(flatten)] snapshot, context }` on the wire/event, `TimelineRecord { #[serde(flatten)] block, context }` on disk. Wire and JSONL shapes are exactly as specified in §3 (old lines/payloads deserialize; a contextless record serializes byte-identically to the old shape). `engine/timeline.rs` went generic (`Serialize`/`DeserializeOwned` + a `HasStart` trait) so the store never references context.
- **Roots registry.** No project→root mapping existed. `context::SharedRoots` (slug → root) is populated by the transcript watcher from the first-`cwd` read it already performs (aliased dirs record under the alias slug); browser-minted projects never appear, hence yield `None` per §4. On native-Windows builds a Unix cwd read through a `\\wsl$` tree is rebased onto that UNC prefix.
- **Settings are JSON.** `settings.rs` is a serde-JSON file; the `toml_edit` merge path named in §7 is the browser mapping store's convention, not settings'. Keys: `contextStrings` (default true), `contextTtlSeconds` (default 60, no UI). The toggle applies live — the orchestrator reads the shared settings handle each wake (this also made the NeuroSkill toggle live, previously next-launch).
- **`observed_at` is unix seconds** (`i64`), matching every other timestamp in the codebase, not a `DateTime<Utc>`.
- **Timeline rows didn't exist.** The expanded view was rail + per-project totals only; it gained a per-block list (`HH:MM–HH:MM project context`) — the "expanded rows" §6 assumed.
- **Subject-spawn seam** is a pure/impure split (`assemble`/`sanitize`/`parse_head` pure and fixture-tested; `git_subject` the only spawn) rather than a trait.

### HookPrompt (added 2026-07-26)

- **Capture rides `WorkEvent.detail`** — the pre-existing semantic-payload field the engine ignores and drops — set by `hooks::hook_to_event` only when the live gate (`context_strings && context_hook_prompts`, read per request in `serve`) is on: a sanitized prompt snippet on `Prompt`, the transcript's head `summary` line on `SessionStart` (bounded 64-line scan, `claude_code::first_summary`). Off = the text is dropped at the receiver, never carried in-process. Harness-injected `UserPromptSubmit` payloads — the prompt opens with an angle-bracket tag (`<task-notification>…`, `<command-name>…`) — are dropped at the same boundary (`is_synthetic_prompt`): they'd otherwise supersede your real prompt moment with plumbing text.
- **Moments are orchestrator-local** (`HashMap<slug, ContextString>`, source `hook_prompt`): harvested from inbound Claude Code `Prompt`/`SessionStart` events (the kind restriction keeps e.g. the synthetic `Select` detail out), last-writer-wins between prompt and summary, cleared on that project's `BlockClosed` — a moment describes the block it arrived in.
- **Priority (§10 ladder, first two rungs live):** a moment for the focused slug wins over git and needs no root (a hook prompt gives context even for a rootless project); git resolves as before when no moment exists.
- **Display-only, pinned by test:** `closing_context` cannot see the moments map, so nothing prompt-derived can reach `timeline.jsonl` (`moments_are_display_only_never_stamped`).
- Only the hooks path captures snippets — the transcript watcher's `SessionStart` doesn't read summaries (no live settings there; hooks are this source's delivery mechanism, as named).

### BrowserTitle (added 2026-07-26)

- **Moments generalized to a shared store** (`context::SharedMoments`, slug → `ContextString`): capture-enabled **receivers record** (the hooks receiver records `HookPrompt` moments from the captured `detail`; the browser receiver records `BrowserTitle` moments from `conversation_title`), the **orchestrator arbitrates and clears** — per-source display gates in `focus_context` (`moment_source_enabled`), cleared on that slug's `BlockClosed`. This replaced the orchestrator-side event harvest HookPrompt shipped with.
- **Extension flow:** `content.js` reads `document.title`, strips the provider suffix (`titleStrip`/`titleBare` per provider in `providers.js`), and includes it in every observation; `background.js` **deletes it before the POST unless the `/raise` poll said `capture_titles`** — the one widget setting drives the sensor, no second options-page knob, and worker restarts default back to off. The receiver re-checks the gate and sanitizes to the shared 120-char cap.
- **Attributed-only:** a title records a moment only when resolution attributed the conversation to a slug — ambient/error/foreign payloads leave no trace. Titles never touch resolution, minting, or focus.
- Last-writer-wins across sources stands (§10 freshest-per-moment): a title ping supersedes an older prompt snippet for the same project and vice versa.
- **Purge on disable:** turning a source's gate off in Settings drops that source's already-stored moments immediately (`context::purge_source`, called from `set_settings` on the gate's true→false edge) — the display gate alone would only hide the text while retaining it in memory until block close, and an opt-in privacy gate must release captured content when revoked.
