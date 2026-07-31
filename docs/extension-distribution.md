# Extension distribution

**Status:** Adopted — 2026-07-26; repo-side changes (§4 sans `key`, §6, §7, §9)
implemented 2026-07-31. Store-side steps (§5, §8, the §4.3 `key` pin) remain.
**Scope:** Release/packaging concern. One manifest change set, one additive daemon field (protocol handshake), no adapter logic changes, no engine changes, no label changes.

## 1. Purpose

`extension/` currently has no distribution story: it is installed by `Load unpacked`, which Chrome treats as a testing affordance, not a channel. Dev-mode extensions nag on restart and can be disabled by Chrome updates or enterprise policy. Self-hosting a signed `.crx` is not an alternative — Chrome only trusts Web Store signatures or enterprise-policy installs, so a self-signed drop fails with `CRX_REQUIRED_PROOF_MISSING`.

This document fixes the channel as **Chrome Web Store, unlisted**, and specifies what must change in the repo before the first submission.

## 2. Decision

**Unlisted Chrome Web Store listing**, with a load-unpacked zip in GitHub releases as the interim and permanent fallback.

Unlisted rather than public because the extension has no standalone value — it is inert without a token pasted from Whence Settings and without a daemon on 18452. A public listing invites installs from people who have never heard of the widget, and the return on that is confused one-star reviews on something that isn't the product. Unlisted gives a shareable link, real signing, and auto-update, with no storefront presence. Review is identical either way; nothing is being dodged.

Auto-update is the load-bearing reason, not install convenience. The extension and the receiver share a wire format on 18452. A stale extension against a newer daemon is a *silent misattribution* failure — the worst category for a sensor whose output is NeuroSkill's ground truth. §6 adds the handshake that makes such a mismatch visible; store distribution is what makes it fixable.

## 3. Invariants

1. **Never `<all_urls>`, never the `tabs` permission.** Tab raising works today via `host_permissions` alone (`chrome.tabs.query` with a `url` filter is authorized by host permissions; `tabs.update`/`windows.update` need no permission). Adding either would move the extension to the slow review track and break the §2 narrative.
2. **No remote code.** All scripts ship in the package. Nothing is fetched and evaluated.
3. **Loopback only.** The two endpoint constants stay on `127.0.0.1`. There is no configurable host.
4. **Inert without the companion app.** No token, no traffic. This is the honest answer to "what does this do on its own," and it must stay true.
5. **`http://127.0.0.1/*` is already minimal.** Chrome match patterns cannot express a port. Do not "narrow" it to `:18452` — the manifest will be rejected.

§9 pins 1, 3, and 5.

## 4. Manifest changes (blocking)

Target state for the head of `extension/manifest.json`. Everything from `host_permissions` down is unchanged.

```json
{
  "manifest_version": 3,
  "name": "Whence: Browser LLM sensor",
  "version": "0.3.0",
  "description": "Feeds browser AI chat project identity into the local Whence widget. Reads the self-declared project only, never chat content.",
  "homepage_url": "<repo URL — fill in at implementation time>",
  "icons": {
    "16": "icons/icon-16.png",
    "32": "icons/icon-32.png",
    "48": "icons/icon-48.png",
    "128": "icons/icon-128.png"
  },
  "permissions": ["storage"],
```

Ordering note: the `icons` key and the four PNGs land in the same commit. A manifest referencing icon paths that do not exist fails `Load unpacked` outright, so this is not a change that can be half-applied.

### 4.1 `name` and `description`

**`name`** — em dash replaced with a colon: `Whence: Browser LLM sensor`. The em dash renders inconsistently across the CWS dashboard and listing surfaces, and the colon is the conventional form for a companion-component title.

The product is still named **Whence**. Only the extension's manifest `name` carries the qualifier, because a store listing has to stand alone in a list of unrelated extensions. Nothing in the widget, the docs, or the daemon adopts the suffixed form.

**`description`** — the current string is roughly 220 characters and CWS caps the field at 132, so the upload fails as-is. The replacement is 125 characters. The long-form version, including the tab-raising behaviour and the producer-side framing, moves to the store listing's detailed description, where there is room for it.

No em dash in the replacement either; a comma carries the clause. (A colon reads wrong in that position — "the self-declared project only: never chat content" parses as a definition rather than a contrast.)

### 4.2 `icons` — missing entirely

Four PNGs under `extension/icons/`, at 16 / 32 / 48 / 128 px, plus the `icons` key above. The 128 is reused as the store icon asset.

Design brief: reuse the widget's existing tray/app mark rather than minting a second identity. This is Whence appearing in a second surface, not a separate product with its own visual language.

### 4.3 `key` — pin the extension ID

Load-unpacked derives an unstable ID per machine. Once a store build exists there are two IDs for one extension, which breaks any doc, support answer, or future native-messaging host that names one.

Procedure (this direction, not the reverse — it's the one guaranteed to work):

1. Upload the first package to CWS as a draft. CWS assigns the item ID.
2. Publish unlisted, download the served `.crx`, extract the public key from its header.
3. Add that key to `manifest.json` as `"key": "<base64>"` and commit it. It is a *public* key — safe in the repo.
4. Every subsequent `Load unpacked` now resolves to the store ID.

The corresponding private key lives only in Google's dashboard. Nothing secret enters the repo.

### 4.4 Version

CWS requires 1–4 dot-separated integers, strictly increasing, never reused. `0.3.0` is valid. Adopt the rule that the extension version is bumped in the same commit as any change to the 18452 payload shape, and that the protocol integer (§6) is bumped only on breaking changes.

Also add `"homepage_url"` pointing at the repo.

## 5. Listing and disclosure checklist

Prepared once, reused on every submission.

**Single purpose statement.** "Detects which AI-chat project the user has filed the current conversation under, and reports that project identity to the user's Whence desktop application over loopback."

**Permission justifications** — the field where reviews are actually won or lost:

| Item | Justification |
|---|---|
| `storage` | Stores the bearer token the user pastes from the companion app's Settings. Nothing else is stored. |
| `https://claude.ai/*`, `https://chatgpt.com/*`, `https://chat.openai.com/*` | Content script reads the project identifier the user themselves assigned to the conversation, from the URL and one DOM anchor. No message content is read. |
| `http://127.0.0.1/*` | Delivers that identifier to the user's own Whence desktop app listening on loopback. Ports cannot be expressed in match patterns; the extension only ever contacts port 18452. |
| No `tabs` permission | Tab activation is performed via host permissions only, deliberately, so the extension cannot enumerate non-provider tabs. Worth stating affirmatively — it reads well. |

**Remote code:** "No, I am not using remote code."

**Data usage:** declare *Website content* (the project-scoped URL and provider-assigned project name) and, if the opt-in title capture is enabled, note it is user-gated and off by default. Certify: not sold, not transferred to third parties, not used for purposes unrelated to single purpose, not used for creditworthiness. All four are true.

**Privacy policy URL:** required, since data handling is declared. A GitHub Pages page from the repo satisfies it. It must state that data reaches only `127.0.0.1` and never a server.

**Code readability:** the package ships unminified source. Link the repo in the listing so the reviewer can diff the package against it — this is the single cheapest way to avoid an obfuscation flag.

**Assets:** 128×128 icon, at least one 1280×800 screenshot (the widget with a browser-attributed row, plus the options page showing the token field).

**Visibility:** Unlisted.

## 6. Protocol handshake (daemon side)

The only code change outside `extension/`. Additive, backward-compatible.

- The observation payload gains `protocol: <u32>`. The `/raise` response gains `protocol: <u32>` (the daemon's own).
- `browser.rs` records the last-seen extension `protocol` and `version` alongside the existing receiver state. Absent field → treat as protocol 1.
- Settings gains a diagnostic line under the browser adapter: extension version, protocol, and one of *current* / *outdated — update from the Web Store* / *newer than this daemon*.

**Invariant:** a mismatch never drops an event and never alters an attribution decision. It degrades to a visible line in Settings. Diagnostic, never evaluative — consistent with the rejected-request counter.

## 7. Repo changes

- `scripts/package-extension.sh` — zips `extension/` (excluding `README.md`, icon sources, anything dotfile) into `whence-extension-v<version>.zip`, reading the version from `manifest.json` so the two cannot drift.
- `extension/README.md` — install section rewritten: store link first, load-unpacked second, with the token-paste step in both. Also fix the dangling `../docs/browser-llm-adapter.md` reference; that doc was deleted post-implementation.
- `whence-spec.md` §5.5 — same dangling reference, same fix. Point at `extension/` and this document.
- `CLAUDE.md` — one line recording the durable call: *extension ships unlisted via CWS; the `key` in the manifest is the pinned public key and must not be regenerated.*

## 8. Sequence

1. Register the CWS developer account now ($5, one-time, per account, unlimited items). Account age is a mild input to review triage and it costs nothing to have it aging.
2. Fix §4.1 and §4.2. These are the only hard blockers.
3. Ship the current release with the packaged zip and load-unpacked instructions, so the clone-the-repo audience isn't gated on Google.
4. Submit unlisted. Expect days-to-weeks; Google has a standing notice about a submission backlog. Do not resubmit to reset the queue — it restarts the clock.
5. On approval, complete §4.3 and commit the key.
6. Land §6 in the same cycle, so the first store-updated users get the mismatch diagnostic.

## 9. Tripwires

Extending the `tripwires.rs` habit to the extension package (a small Rust test parsing `extension/manifest.json` keeps it in one harness):

| Invariant | Tripwire |
|---|---|
| §3.1 no broad permissions | Assert `permissions == ["storage"]` and `"tabs"` absent; assert no host pattern contains `<all_urls>` |
| §3.3 loopback only | Grep `background.js` for endpoint constants; assert both hosts are `127.0.0.1` |
| §3.5 port-free match pattern | Assert no `host_permissions` entry matches `:\d` |
| §4.1 store limit | Assert `description.len() <= 132` |
| §4.4 version coupling | Assert manifest `version` and the protocol constant are both present and parse |

Each failure message names this document's section.

## 10. Non-goals

- **Public listing.** Revisit only if Whence itself has an audience that would search for the extension independently. It won't before v2.
- **Firefox / AMO.** AMO will sign an XPI for self-distribution, which is the one genuine store-free channel that exists — worth knowing, not worth doing until someone asks for Firefox.
- **Edge Add-ons.** Takes the same package; Edge and Brave users can install from CWS today. Redundant.
- **Enterprise policy install.** Solves a fleet problem Whence does not have.
