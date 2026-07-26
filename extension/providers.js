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
// One host can serve more than one provider surface (claude.ai serves chat AND
// Claude Design). Such an entry carries a `pathPrefix` and is selected by
// host+path, specificity-ordered (a pathPrefix entry wins over the bare-host
// fallback). The daemon mirrors this in `provider_for_url`; the rule must match on
// both sides.
//
// Loaded before content.js in the same content-script world, so `WHENCE_PROVIDERS`
// (declared with `var` to share across files) is visible there.

// eslint-disable-next-line no-unused-vars, no-var
var WHENCE_PROVIDERS = [
  {
    // Claude Design (claude.ai/design) — a distinct product on the claude.ai host,
    // its own named projects, canvas, and metering. A separate provider id (its own
    // store keyspace); converges with chat/fs at the slug layer. Listed BEFORE the
    // bare `claude` entry: both match the host, but this one gates on `pathPrefix`
    // and is the more specific surface, so it wins on /design pages.
    provider: "claude-design",
    hostPattern: /(^|\.)claude\.ai$/i,

    // Gates this entry to /design pages (and only there). Same host as chat, so the
    // path is the disambiguator; the daemon's provider_for_url applies the same rule.
    pathPrefix: "/design",

    // A Design project page is /design/p/{id} (the {id} is the stable project/canvas
    // key; a `?file=` query selects a sub-document and is dropped by normalization).
    // The id is in the URL, so it's Level-1/Level-2 keyable with no DOM read needed.
    projectUrlPattern: /\/design\/p\/([^/?#]+)/,

    // The Design SPA has no <header> breadcrumb and no project anchor (the sidebar is
    // a separate app); the readable project name is a titled span. Used for the NAME
    // (and to confirm "grouped"); the id comes from the URL above, not an href — so no
    // anchorHrefPattern.
    projectAnchorSelector: '[data-testid="project-title"]',

    // The Design chat-panel composer.
    inputSelector: '[data-testid="chat-composer-input"]',

    // Confirmed generating: the send control flips to `<button title="Stop">` (an
    // ai-Stop icon) while the canvas renders; idle shows `title="Send (Enter)"`, so this
    // can't false-positive on the send button. The extra selectors are forward-compat.
    streamingSelector:
      'button[title="Stop"], button[data-testid="chat-stop-button"], button[aria-label*="Stop" i]',

    // Assistant turns in the Design chat panel (user rows are plain `data-index`
    // blocks; assistant replies are wrapped in `.om-assistant-group`). A count
    // increment = a completed round = you acted.
    turnSelector: ".om-assistant-group",

    // Conversation title (opt-in context source; README "Conversation titles"):
    // document.title minus the provider suffix. `titleBare` names the generic
    // untitled forms that count as "no title", not a title.
    titleStrip: /\s*[-–—|·]\s*Claude(\s+Design)?\s*$/i,
    titleBare: /^(Claude|Claude Design|New (chat|design))$/i,
  },

  {
    provider: "claude",
    // Robust: which host this entry serves. No pathPrefix → the host's default
    // surface (chat) and the fallback for any non-/design claude.ai page.
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

    // Conversation title (opt-in context source): document.title minus " - Claude".
    titleStrip: /\s*[-–—|·]\s*Claude\s*$/i,
    titleBare: /^(Claude|New chat)$/i,
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

    // Conversation title (opt-in context source): document.title minus " | ChatGPT".
    titleStrip: /\s*[-–—|·]\s*ChatGPT\s*$/i,
    titleBare: /^(ChatGPT|New chat)$/i,
  },
];
