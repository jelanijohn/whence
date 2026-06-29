// Whence browser sensor — background service worker.
//
// Two jobs, both loopback-only:
//   1. Relay each content-script observation to Whence's browser receiver. The content
//      script can't POST to the loopback receiver itself (page-origin CORS blocks it),
//      so it forwards here and this worker relays it. Fire-and-forget.
//   2. Poll the receiver's `/raise` back-channel for tabs the widget asked to raise
//      (the user clicked a browser source row), and activate the matching tab. We WRITE
//      focus; we never READ which tab/window is foregrounded — the target is matched
//      purely by the URL we were handed (spec §1/§2 stays intact).

const WHENCE_ENDPOINT = "http://127.0.0.1:18452/browser";
const WHENCE_RAISE_ENDPOINT = "http://127.0.0.1:18452/raise";
const WHENCE_DEBUG = false; // flip on to trace relaying in the service-worker console

// Poll cadence for the raise back-channel. A click is only useful for a moment, so
// ~1.5s latency is imperceptible while staying gentle on the loopback.
const POLL_MS = 1500;

// The provider tabs we may raise — same hosts as the content-script matches. Used as
// `chrome.tabs.query` match patterns; reading these tabs' URLs is authorized by the
// extension's existing `host_permissions` (no broad `"tabs"` permission needed).
const PROVIDER_GLOBS = [
  "https://claude.ai/*",
  "https://chatgpt.com/*",
  "https://chat.openai.com/*",
];

// --- 1. Observation relay -----------------------------------------------------------

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (!msg || msg.type !== "whence:observation") return;
  // Return `true` and call sendResponse so the message channel stays open until the
  // fetch settles — this keeps the MV3 service worker alive long enough to finish the
  // POST. Without it the worker can be terminated the instant this listener returns,
  // cutting the request (the bug that made relaying intermittent).
  fetch(WHENCE_ENDPOINT, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(msg.payload),
    keepalive: true,
  })
    .then((r) => {
      if (WHENCE_DEBUG) console.log("[whence:bg] relayed", r.status, msg.payload.url);
      sendResponse({ ok: true, status: r.status });
    })
    .catch((e) => {
      if (WHENCE_DEBUG) console.log("[whence:bg] relay FAILED:", e && e.message);
      sendResponse({ ok: false });
    });
  return true;
});

// --- 2. Raise back-channel ----------------------------------------------------------

// Mirror src-tauri/src/adapters/browser.rs `normalize_url` exactly: drop the fragment
// and query, lowercase scheme + host (NOT the path — conversation ids are
// case-sensitive), and trim trailing slashes. The source id we're handed is already
// normalized this way, so we normalize each candidate tab and compare for equality.
function normalizeUrl(url) {
  url = url.split("#")[0];
  url = url.split("?")[0];
  let scheme = "";
  let rest = url;
  const si = url.indexOf("://");
  if (si !== -1) {
    scheme = url.slice(0, si);
    rest = url.slice(si + 3);
  }
  let host = rest;
  let path = null;
  const pi = rest.indexOf("/");
  if (pi !== -1) {
    host = rest.slice(0, pi);
    path = rest.slice(pi + 1);
  }
  let out = "";
  if (scheme) out += scheme.toLowerCase() + "://";
  out += host.toLowerCase();
  if (path !== null) out += "/" + path;
  return out.replace(/\/+$/, "");
}

async function raiseTab(targetUrl) {
  // Match ONLY by the URL we were handed — never by which tab/window is foregrounded.
  const tabs = await chrome.tabs.query({ url: PROVIDER_GLOBS });
  const hit = tabs.find((t) => t.url && normalizeUrl(t.url) === targetUrl);
  if (!hit) {
    if (WHENCE_DEBUG) console.log("[whence:bg] no open tab for", targetUrl);
    return;
  }
  await chrome.tabs.update(hit.id, { active: true });
  if (hit.windowId != null) await chrome.windows.update(hit.windowId, { focused: true });
  if (WHENCE_DEBUG) console.log("[whence:bg] raised", targetUrl);
}

let polling = false;
async function pollOnce() {
  try {
    const r = await fetch(WHENCE_RAISE_ENDPOINT);
    if (!r.ok) return;
    const { raise = [] } = await r.json();
    for (const url of raise) await raiseTab(url);
  } catch (_) {
    // Whence not running / browser adapter off / port taken — ignore, retry next tick.
  }
}

function startPolling() {
  if (polling) return;
  polling = true;
  (function loop() {
    pollOnce().finally(() => setTimeout(loop, POLL_MS));
  })();
}

// Keepalive: a connected port from any provider tab's content script resets the SW's
// idle timer, so the poll loop stays alive exactly while a raisable tab exists. When
// the last provider tab closes the SW may sleep — correct, since there's then nothing
// to raise (the queue simply drains on the next wake).
chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== "whence-keepalive") return;
  port.onMessage.addListener(() => {}); // each ping resets the idle timer
  startPolling();
});

// Start polling on every worker (re)evaluation and on browser startup, so a wake from
// any event (a relayed observation, a keepalive connect) resumes the loop.
chrome.runtime.onStartup.addListener(startPolling);
startPolling();
