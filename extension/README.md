# Whence — Browser LLM sensor (extension)

A first-party Whence browser extension that makes browser-hosted AI chat
(claude.ai, chatgpt.com) visible to the Whence widget, the same way the Claude Code
transcript watcher and hooks make CLI sessions visible. See
`../docs/browser-llm-adapter.md` for the full design and `../whence-spec.md` §§1–2 for
why this stays on the **producer** side of the surveillance ban.

## What it reads (and doesn't)

It captures only a **self-declared organizational artifact** — the project you filed
a chat under — plus transient status:

| Captured (sent to Whence) | Never captured |
|---|---|
| the project-scoped URL | window / app / tab focus, any focus-state signal |
| `provider` (from host) | reading time, scroll, mouse, dwell, click, idle |
| `provider_project_id` / `…_name` | prompt or response **content** |
| turn **count** (not content) | clipboard, form values, keystrokes |
| `streaming` / `input_detected` booleans | non-provider pages |

Attribution never derives from which window is foregrounded — reading the project
marker is the same category as reading a repo marker, not the surveillance category
§2 rejects.

## Install (load unpacked)

1. In the Whence widget settings, enable **Browser → Browser AI sessions** and save
   (the daemon then listens on `127.0.0.1:18452`).
2. Open your browser's extensions page:
   - Chrome/Edge/Brave: `chrome://extensions`
   - enable **Developer mode**, click **Load unpacked**, and select this `extension/`
     folder.
3. Visit claude.ai or chatgpt.com. A chat filed under a project shows up as a session
   row in the widget, attributed to that project's slug.

The receiver is loopback-only and unauthenticated by design (principle #4); the
extension only ever POSTs to `127.0.0.1`.

## Brittleness (read this before filing a bug)

Extraction is **brittle by construction** (§8). The provider hostname is the only
robust field; the id/name/turn/streaming selectors are best-effort and **will break
silently** when claude.ai or chatgpt.com change their markup. When attribution stops
working:

- All the selectors live in **`providers.js`** — the single place to patch.
- Failure degrades safely: a missed extraction becomes an *ambient* (unattributed)
  session or a logged daemon error, never a *wrong* attribution.

## Files

- `manifest.json` — MV3 manifest (host permissions, content script, service worker).
- `providers.js` — the per-provider config table (hosts, URL patterns, selectors).
  **All brittleness is centralized here.**
- `content.js` — reads the page per the config, forwards observations to the worker.
- `background.js` — relays observations to the loopback receiver.

To add a provider, add an entry in `providers.js` **and** extend `provider_for_host`
in `src-tauri/src/adapters/browser.rs` (the daemon re-derives the provider from the
host, so the two must agree).
