// Whence browser sensor — the single provider config table (spec §6).
//
// ALL brittleness lives here. Provider markup changes silently break id/name/turn/
// streaming detection until these selectors are patched — the same fragility
// NeuroSkill's own extension documents (§8). The hostname is the only robust field;
// everything else is best-effort and degrades safely (a missed extraction yields an
// ambient session or a logged error on the daemon, never a wrong attribution).
//
// To add a provider: add an entry here AND extend `provider_for_host` in
// src-tauri/src/adapters/browser.rs (the daemon re-derives the provider from the
// host, so the two must agree).
//
// Loaded before content.js in the same content-script world, so `WHENCE_PROVIDERS`
// (declared with `var` to share across files) is visible there.

// eslint-disable-next-line no-unused-vars, no-var
var WHENCE_PROVIDERS = [
  {
    provider: "claude",
    // Robust: which host this entry serves.
    hostPattern: /(^|\.)claude\.ai$/i,

    // The CURRENT chat's project link lives in the page <header> (a breadcrumb back to
    // the project), carrying BOTH the stable id (in its href) and the readable name
    // (its text). It MUST be scoped to the header: the sidebar <nav> lists *every*
    // project, so an unscoped `a[href*="/project/"]` grabs whichever project is first
    // in the sidebar, not the one this chat belongs to.
    //   <header> … <a href="/project/{id}">{name}</a> … </header>
    projectAnchorSelector: 'header a[href*="/project/"]',
    anchorHrefPattern: /\/project\/([^/?#]+)/,

    // Fallback: the project's own page is /project/{id} — pull the id straight from
    // the URL even when the anchor isn't on the page.
    projectUrlPattern: /\/project\/([^/?#]+)/,

    // The composer (contenteditable). Non-empty → you're typing (input_detected).
    inputSelector: 'div[contenteditable="true"]',

    // A "stop"/abort control is present only while the model is generating.
    streamingSelector:
      'button[aria-label*="Stop" i], button[aria-label*="stop response" i]',

    // Turn counter: assistant message blocks. Count, don't read — the count is all
    // the daemon needs (a turn increment = you acted).
    turnSelector: '[data-testid="assistant-message"], div.font-claude-message',
  },

  {
    provider: "chatgpt",
    hostPattern: /(^|\.)(chatgpt\.com|chat\.openai\.com)$/i,

    // ChatGPT "Projects" live under /g/g-p-{id}-{slug}; the id comes robustly from the
    // URL (projectUrlPattern below), so the anchor is only needed for the readable
    // name — scoped to the header so it's the current project, not a sidebar entry.
    projectAnchorSelector: 'header a[href*="/g/g-p-"]',
    anchorHrefPattern: /\/g\/(g-p-[^/?#]+)/,

    projectUrlPattern: /\/g\/(g-p-[^/?#]+)/,

    inputSelector: "#prompt-textarea, textarea",

    streamingSelector:
      'button[data-testid="stop-button"], button[aria-label*="Stop" i]',

    turnSelector: "[data-message-author-role]",
  },
];
