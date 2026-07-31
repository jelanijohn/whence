# Whence — Browser LLM sensor (extension)

A first-party Whence browser extension that makes browser-hosted AI chat
(claude.ai, chatgpt.com) visible to the Whence widget, the same way the Claude Code
transcript watcher and hooks make CLI sessions visible. The daemon half lives in
`../src-tauri/src/adapters/browser.rs`; see `../docs/extension-distribution.md` for
how this package ships and `../whence-spec.md` §§1–2 for why it stays on the
**producer** side of the surveillance ban.

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
| `conversation_title` — **opt-in, default off** (see below) | |

Attribution never derives from which window is foregrounded — reading the project
marker is the same category as reading a repo marker, not the surveillance category
§2 rejects.

### Conversation titles (opt-in scope extension)

With **Settings → Context → Conversation titles** enabled in Whence, the sensor also
sends the conversation's **title** (one `document.title` read, provider suffix
stripped) so the widget can show *what* the focused chat is about
(`docs/context-strings.md` §10, `BrowserTitle`). Because providers auto-title chats
from their content, this is **content-derived** and therefore off by default and
double-gated:

- The daemon advertises the setting on the `/raise` poll response
  (`capture_titles`); the service worker **strips the title from every observation
  unless that flag is on** — nothing title-shaped leaves the browser while the
  setting is off (worker restarts default back to off until the next poll).
- The receiver drops titles again unless the setting is on (defense in depth), and
  what it keeps is **display-only**: shown beside the live focus, never written to
  the timeline or any NeuroSkill label.

Attribution is entirely unaffected — the title never feeds a mapping, mint, or
focus decision.

## Raising a tab (the back-channel)

Clicking a browser source row in the widget raises that tab. The daemon enqueues the
target URL; the service worker polls `127.0.0.1:18452/raise` (loopback-only,
bearer-gated, no page content) and activates the tab whose URL matches. The match
is purely on the handed-in URL — the extension never reads which tab/window is
foregrounded, so this *writes* focus without ever *reading* OS focus (§1/§2). A
content-script keepalive port keeps the worker alive (and the poll running) only while
a provider tab is open. No extra permissions: `host_permissions` already covers both
the loopback poll and activating the provider tabs.

## Install

Either channel — first, in the Whence widget settings, enable **Browser →
Browser AI sessions** and save (the daemon then listens on `127.0.0.1:18452`).

### Chrome Web Store (preferred — auto-updates)

The extension is published **unlisted** (it's inert without the widget, so it has
no storefront presence; see `../docs/extension-distribution.md` §2).

1. Install from the store link in the Whence release notes. *(Link lands with the
   first approved submission — until then use load-unpacked below.)*
2. Open the extension's **options** page and paste the receiver token from the
   widget's Settings → **Receiver auth** (one time; re-paste after a rotation).
3. Visit claude.ai or chatgpt.com. A chat filed under a project shows up as a
   session row in the widget, attributed to that project's slug.

Auto-update matters more than convenience here: the extension and the daemon share
a wire format, and a stale extension is a silent-misattribution risk. The daemon
surfaces a version/protocol diagnostic in Settings → Browser when the pair drift.

### Load unpacked (fallback, or from a release zip)

1. Grab `whence-extension-v<version>.zip` from a GitHub release and unzip it, or
   use this `extension/` folder straight from a checkout.
2. Open your browser's extensions page:
   - Chrome/Edge/Brave: `chrome://extensions`
   - enable **Developer mode**, click **Load unpacked**, and select the folder.
3. Paste the receiver token into the options page and visit a provider, exactly as
   above. Note Chrome treats dev-mode extensions as a testing affordance — expect
   the on-restart nag, and no auto-update.

The receiver is loopback-only and **bearer-gated**: every request carries the
token as an `Authorization` header, so other local processes (or a drive-by web
page POSTing at localhost) can't forge observations or read the raise queue.
Without the token the receiver answers 401 and the widget's Settings show a
rejected-request counter. The extension only ever POSTs to `127.0.0.1`.

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
- `content.js` — reads the page per the config, forwards observations to the worker,
  and holds a keepalive port so the worker's raise-poll stays alive while the tab is open.
- `background.js` — relays observations to the loopback receiver (attaching the
  bearer token), and polls the `/raise` back-channel to activate a tab the widget
  asked to raise.
- `options.html` / `options.js` — the one-field options page holding the receiver
  token (`chrome.storage.local`).
- `icons/` — the widget's app mark at the four manifest sizes (16/32/48/128), so
  Whence keeps one visual identity across surfaces.

To add a provider, add an entry in `providers.js` **and** extend `provider_for_host`
in `src-tauri/src/adapters/browser.rs` (the daemon re-derives the provider from the
host, so the two must agree).
