// Whence browser sensor — content script (spec §3, content-script half).
//
// Producer-side and narrow (§1/§6): reads only the project-scoped URL, the provider's
// own project id + name, a turn COUNT, and the streaming/input booleans. It never
// reads window/app focus, dwell, scroll, clicks, or any chat content (prompts or
// responses). It extracts a self-declared artifact — the project the user filed the
// chat under — and ships it; the DAEMON owns every mapping lookup, mint, and the
// attribution decision.
//
// The content script cannot reach the loopback receiver directly (page-origin CORS),
// so it forwards each observation to the background service worker, which POSTs it.

(() => {
  "use strict";

  // Flip to false once selectors are dialed in. Logs every observation to the page
  // console (content-script logs surface in the host page's DevTools), so you can see
  // what the sensor reads on real provider pages.
  const WHENCE_DEBUG = false;

  // Whether `path` lies under `prefix` at a segment boundary: `prefix` itself or
  // `prefix/…`, never `prefixfoo` (so "/design" matches "/design" and "/design/x"
  // but not "/designs"). Mirrors the daemon's `path_has_prefix`.
  function pathHasPrefix(path, prefix) {
    return path === prefix || (path.startsWith(prefix) && path[prefix.length] === "/");
  }

  // Provider selection is host+path-aware (claude.ai serves chat AND Claude Design):
  // an entry matches when its host matches AND, if it declares a `pathPrefix`, the
  // path is under it. Specificity-ordered (§2): a pathPrefix entry is more specific
  // than the bare-host fallback, so it wins when both match — order-independently.
  const providers = typeof WHENCE_PROVIDERS !== "undefined" ? WHENCE_PROVIDERS : [];
  const matches = providers.filter(
    (p) =>
      p.hostPattern.test(location.host) &&
      (!p.pathPrefix || pathHasPrefix(location.pathname, p.pathPrefix)),
  );
  const cfg = matches.find((p) => p.pathPrefix) || matches[0];
  if (!cfg) {
    if (WHENCE_DEBUG) console.log("[whence] no provider config matches host", location.host);
    return; // not an allowlisted provider page
  }
  if (WHENCE_DEBUG) console.log("[whence] sensor active for provider", cfg.provider);

  // Throttle: re-observe at most every SAMPLE_MS, and only POST when the payload
  // actually changed (so a quiet tab is silent — no behavioral polling, just state).
  const SAMPLE_MS = 2000;
  let lastSent = "";
  let timer = null;

  const text = (el) => (el && (el.textContent || el.value || "")).trim();

  function firstMatch(url, re) {
    if (!re) return null;
    const m = url.match(re);
    return m ? m[1] : null;
  }

  // Decide the ambient/error discriminator (§3) from PRESENCE of a project anchor,
  // plus whatever id/name we can read. Best-effort by construction.
  function extract() {
    const url = location.href;

    let provider_project_id = firstMatch(url, cfg.projectUrlPattern);
    let provider_project_name = null;
    let project_state = provider_project_id ? "grouped" : "ambient";

    let anchor = null;
    try {
      anchor = document.querySelector(cfg.projectAnchorSelector);
    } catch {
      anchor = null;
    }

    if (anchor) {
      // An anchor is present → this chat IS filed under a project.
      project_state = "grouped";
      try {
        const href = anchor.getAttribute("href") || "";
        provider_project_id = provider_project_id || firstMatch(href, cfg.anchorHrefPattern);
        const name = text(anchor);
        if (name) provider_project_name = name;
        // Anchor present but neither id nor name came out → extraction broke (§3 error:
        // the provider's DOM changed and selectors need a patch).
        if (!provider_project_id && !provider_project_name) project_state = "error";
      } catch {
        project_state = "error";
      }
    }

    let streaming = false;
    try {
      streaming = cfg.streamingSelector ? !!document.querySelector(cfg.streamingSelector) : false;
    } catch {
      streaming = false;
    }

    let input_detected = false;
    try {
      input_detected = cfg.inputSelector ? text(document.querySelector(cfg.inputSelector)).length > 0 : false;
    } catch {
      input_detected = false;
    }

    let turn_count;
    try {
      turn_count = cfg.turnSelector ? document.querySelectorAll(cfg.turnSelector).length : undefined;
    } catch {
      turn_count = undefined;
    }

    // Conversation title — the one extra DOM read of the opt-in BrowserTitle
    // context source (README "Conversation titles"). Read here, but the BACKGROUND
    // worker strips it from the observation unless Whence said titles are on (the
    // /raise-carried `capture_titles` flag), so nothing title-shaped leaves the
    // browser while the setting is off. Generic untitled forms count as no title.
    let conversation_title = null;
    try {
      let t = (document.title || "").trim();
      if (cfg.titleStrip) t = t.replace(cfg.titleStrip, "").trim();
      if (t && !(cfg.titleBare && cfg.titleBare.test(t))) {
        conversation_title = t.slice(0, 300); // daemon re-caps at the display bound
      }
    } catch {
      conversation_title = null;
    }

    return {
      url,
      provider: cfg.provider,
      provider_project_id,
      provider_project_name,
      project_state,
      streaming,
      input_detected,
      turn_count,
      conversation_title,
    };
  }

  function tick() {
    let payload;
    try {
      payload = extract();
    } catch {
      return;
    }
    const key = JSON.stringify(payload);
    if (key === lastSent) return; // nothing changed — stay quiet
    lastSent = key;
    if (WHENCE_DEBUG) console.log("[whence] observation →", payload);
    try {
      const p = chrome.runtime.sendMessage({ type: "whence:observation", payload });
      if (p && typeof p.then === "function") {
        p.then((res) => {
          if (WHENCE_DEBUG) console.log("[whence] relay result", res);
        }).catch((e) => {
          if (WHENCE_DEBUG) console.log("[whence] relay error:", e && e.message);
        });
      }
    } catch (e) {
      // Background worker asleep/reloading — the next tick retries.
      if (WHENCE_DEBUG) console.log("[whence] sendMessage threw:", e && e.message);
    }
  }

  function schedule() {
    if (timer) return;
    timer = setTimeout(() => {
      timer = null;
      tick();
    }, SAMPLE_MS);
  }

  // Re-sample on DOM mutations (debounced) and as a slow heartbeat. A MutationObserver
  // on the whole document is coarse but cheap given the debounce + change-gate above.
  const observer = new MutationObserver(schedule);
  observer.observe(document.documentElement, {
    subtree: true,
    childList: true,
    attributes: true,
    attributeFilter: ["aria-label", "data-testid", "href"],
  });

  // SPA navigations don't reload the page; poll the URL as a fallback trigger.
  let lastUrl = location.href;
  setInterval(() => {
    if (location.href !== lastUrl) {
      lastUrl = location.href;
      tick();
    } else {
      schedule();
    }
  }, SAMPLE_MS);

  // Keepalive port — keeps the background service worker (and its `/raise` poll) alive
  // while this provider tab is open, so a click in the widget can raise this tab within
  // a poll cadence even when the tab is otherwise quiet. Self-healing: reconnects if the
  // worker recycled the port. Carries no data — only its liveness matters.
  let keepalivePort = null;
  function connectKeepalive() {
    try {
      keepalivePort = chrome.runtime.connect({ name: "whence-keepalive" });
      keepalivePort.onDisconnect.addListener(() => {
        // Touch lastError so a bfcache-severed port doesn't log "Unchecked
        // runtime.lastError" — expected on navigation, and the interval reconnects.
        void chrome.runtime.lastError;
        keepalivePort = null;
      });
    } catch (_) {
      keepalivePort = null; // worker reloading — the interval below retries.
    }
  }
  connectKeepalive();
  setInterval(() => {
    if (!keepalivePort) connectKeepalive();
    try {
      keepalivePort && keepalivePort.postMessage({ t: "ping" });
    } catch (_) {
      keepalivePort = null; // port died mid-send — reconnect next tick.
    }
  }, 20000); // < the 30s SW idle timer

  tick(); // first observation
})();
