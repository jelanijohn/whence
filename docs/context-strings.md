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

- **Hook prompt snippet / transcript summary** (`HookPrompt`): `UserPromptSubmit` already carries the prompt text, and Claude Code writes short session summaries into the transcript JSONL that the hook's `transcript_path` points at. Content-derived → opt-in; block persistence needs its own decision (the A2 raw-capture posture applies).
- **Browser conversation title** (`BrowserTitle`): one DOM read, but the extension's declared capture-surface table excludes prompt/response-derived content. Adding titles means amending that table in the extension docs first — a documented scope change, not just shipped code.
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
