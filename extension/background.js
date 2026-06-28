// Whence browser sensor — background service worker.
//
// The content script can't POST to the loopback receiver (page-origin CORS blocks
// it), so it forwards each observation here and this worker relays it to Whence's
// browser receiver. Fire-and-forget: a failed POST (Whence not running, browser
// adapter off, port taken) is swallowed — the next observation retries, and a missing
// Whence must never disrupt the page.

const WHENCE_ENDPOINT = "http://127.0.0.1:18452/browser";
const WHENCE_DEBUG = false; // flip on to trace relaying in the service-worker console

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
