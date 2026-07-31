# Whence — WAID Ingestion Contract

*The contract for WAID — or any local consumer — reading Whence's focus
timeline. This documents the **Whence-side obligations only**: which files
exist, what a record means, and what a reader may rely on. The
intention-vs-reality *view* built on top of this data is WAID-repo work
([`intention-vs-reality.md`](intention-vs-reality.md)) and is out of scope
here. Decision record: `whence-spec.md` §14 · 9.*

> [!NOTE]
> **Status:** contract in force as of 2026-07-31. It is written to be true both
> before and after A3 (timeline corrections,
> [`a2-a5-a3-a4-spec.md`](a2-a5-a3-a4-spec.md)) lands: a pre-A3 consumer
> implements the resolution rule trivially, because an absent corrections file
> means an empty corrections set — by contract, not by accident.

## 1. Scope

Whence promises a readable, append-only record of closed focus blocks, plus
(post-A3) a corrections log that re-attributes or drops blocks after the fact.
Nothing else: no push, no API, no aggregation. The consumer reads files.

## 2. Files

| File | Exists | Contents |
|---|---|---|
| `<app_data_dir>/timeline.jsonl` | always (after first block) | one `TimelineRecord` per line, append-only. **Whence is the single writer.** |
| `<app_data_dir>/corrections.jsonl` | only once A3 lands | one correction per line, append-only. **Absent file ⇒ no corrections**, by contract. |

## 3. Record schema

A `TimelineRecord` is the engine's `FocusBlock` flattened, plus an optional
context stamp applied at block close:

```jsonc
{
  "project": "whence",        // the shared slug — the join key (§5)
  "start": 1753948800,        // Unix seconds, block open
  "end": 1753952400,          // Unix seconds, block close
  "eventCount": 41,           // WorkEvents attributed to the block
  "meanConfidence": 0.93,     // mean signal confidence, 0..1
  "context": {                // OPTIONAL — display-only; consumers may ignore
    "text": "main · fix debounce",
    "source": "git"
  }
}
```

**Stability promise: additive-only.** New fields will be optional; existing
fields never change type or meaning; consumers must ignore unknown fields.
(`context` is itself the first exercise of that promise — older lines lack the
key entirely.)

## 4. Resolution rule

To materialize the effective timeline:

1. Read `timeline.jsonl` in file order.
2. Read `corrections.jsonl` if it exists (absent ⇒ empty set).
3. Apply corrections **latest-wins**, keyed on `(project, start)` — the
   *original* slug, i.e. the one written in the timeline line being corrected.
4. Skip blocks a correction marks `drop`ped.
5. Dedupe on `(project, start)`.

Pre-A3, steps 2–4 are no-ops and the rule collapses to read + dedupe.

## 5. Join key

`project` is the shared slug across the ecosystem: every Whence surface
resolves through one `slugify` (`src-tauri/src/adapters/mod.rs`), and it is the
same normalization as WAID's `slugify(brief.name)` index in brief-peek
([`waid-brief-peek.md`](waid-brief-peek.md) §3). This contract is that join,
reversed direction: brief-peek carries WAID's intention into Whence; this file
carries Whence's attributed reality into WAID.

## 6. Reader obligations

- **Open read-only. Never lock the file** — Whence appends live while you read.
- **Tolerate garbage:** skip lines that don't parse, including a truncated tail
  line (Whence may have crashed mid-write). Whence's own readers do the same.
- **Treat file order as append order.** Blocks are written on close, so the
  order is close-time, not necessarily start-time.
- **Ignore unknown fields** (§3), and treat `context` as display-only prose —
  never parse or join on it.

## 7. Honest limitations

- **The currently-open block is not in the file.** Blocks persist on close, so
  a "today" view undercounts by exactly the live block. Revisit only if this
  proves to matter in the WAID view (§9).
- **Blocks open at Whence shutdown are lost entirely** — there is no
  crash-recovery journal for the open block.
- **No retro-correction downstream.** NeuroSkill labels and timeline lines
  already written stay written; A3 corrects the *record* via the corrections
  log, not history already consumed elsewhere.

## 8. Discovery

`app_data_dir` is Tauri's per-app data dir for the identifier
`com.jelanijohn.whence`. Typical defaults — **the implementer verifies the
actual path on the dev machine rather than trusting this table**:

| Platform | Typical path |
|---|---|
| Windows | `%APPDATA%\com.jelanijohn.whence` |
| Linux | `~/.local/share/com.jelanijohn.whence` |
| macOS | `~/Library/Application Support/com.jelanijohn.whence` |

Cross-boundary discovery (WAID on one side of WSL2, Whence's data dir on the
other) is a WAID-side concern; the pattern to copy is Whence's own
Windows-host token walk, `src-tauri/src/neuroskill/client.rs::resolve_token_path`.

## 9. Non-goals

- No push, API, or IPC — the consumer polls/reads files on its own schedule.
- No Whence-side aggregation (daily totals, per-project rollups) — that's the
  view's job.
- No schema negotiation or versioning handshake — the additive-only promise
  (§3) is the whole compatibility story.
- No live-snapshot endpoint for the open block — revisit only if the §7
  undercount proves to matter in the WAID view.
