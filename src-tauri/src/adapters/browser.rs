//! Browser LLM adapter (opt-in) — closes the browser coverage gap.
//!
//! Claude Code transcripts and Ollama cover CLI/agent and local-model use;
//! browser-hosted AI chat (claude.ai, chatgpt.com, …) was invisible. A first-party
//! Whence **browser extension** (see `extension/`) reads the project-scoped URL and
//! the provider's own project identity from the page and POSTs it to the loopback
//! receiver here — the same ingestion shape as the Claude Code hooks receiver. It
//! reads a *self-declared artifact* (the project the user filed the chat under),
//! never window/app focus or behavioral signals, so it stays on the producer side of
//! the §2/§15 surveillance ban.
//!
//! **Originating-capable (§2).** Unlike the terminal corroborator, the browser
//! adapter self-attributes from the provider's project identity and can *mint* a
//! project. It emits full-confidence events; there is no temporal inheritance.
//!
//! **Resolution (§3) is the daemon's job**, not the content-script's: the extension
//! ships `{ url, provider_project_id, provider_project_name, … }`; this module owns
//! every mapping lookup, backfill, and mint against [`MappingStore`]:
//!
//! ```text
//! url → host not in allowlist .............. Ignore (END)
//!     → normalized URL in store (Level-1) .. Attributed(slug)
//!     → else use DOM-read project:
//!         → state = error .................. Error (END; provider DOM changed)
//!         → no project anchor .............. Ambient (project = null)
//!         → (provider, id) in store (L2) ... Attributed(slug); backfill URL
//!         → else ........................... Mint slugify(name); backfill URL
//! ```
//!
//! **Ambient is dropped for v1.** A chat with no provider project (e.g.
//! `claude.ai/new`) resolves to `project = null`. The spec (§3) would emit it as an
//! ambient session node, but Whence's engine/timeline are keyed on a non-null slug,
//! so honoring that needs a null-project session identity in the engine — deferred.
//! For now an ambient (or error) outcome simply produces no event, exactly like
//! unattributed Ollama activity.
//!
//! Three layers, mirroring the rest of `adapters`:
//!   * [`event_for`] — **pure, fixture-tested**: an attributed slug + payload → the
//!     `WorkEvent` (status/focus kind). The clock comes in as a parameter.
//!   * [`resolve`] — the §3 decision over a [`MappingStore`] (in-memory in tests).
//!   * [`serve`] — the thin impure shell: bind `tiny_http`, read bodies, drive the
//!     two above, send. It owns the per-conversation turn-count memory.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use super::browser_map::MappingStore;
use super::{Surface, WorkEvent, WorkKind};

/// Pending "raise this tab" requests — normalized conversation URLs the widget asked
/// to surface, drained by the extension's poll (`GET /raise`). Loopback, in-memory,
/// bounded: a manual navigation affordance, not a log. Shared between a Tauri command
/// (writer, via `enqueue_raise`) and the receiver thread (drainer).
pub type RaiseQueue = Arc<Mutex<VecDeque<String>>>;

/// Cap on pending raises — a backstop against an extension that never polls (Whence
/// not running, browser closed). A click is only ever useful for a few seconds, so a
/// small bound is plenty; the oldest fall off.
const RAISE_QUEUE_CAP: usize = 16;

/// Enqueue a normalized URL for the extension to raise. Newest-click-wins: a repeated
/// target moves to the back rather than queueing twice. A poisoned lock is swallowed
/// (the raise is simply lost) — never panic a command over a navigation nicety.
pub fn enqueue_raise(q: &RaiseQueue, url: String) {
    if let Ok(mut q) = q.lock() {
        q.retain(|u| u != &url);
        q.push_back(url);
        while q.len() > RAISE_QUEUE_CAP {
            q.pop_front();
        }
    }
}

/// The content-script payload. Forward-compatible (unknown fields ignored), like the
/// Claude hook payload — provider markup churns, so every field is optional and a
/// missing one degrades gracefully rather than failing the decode.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BrowserPayload {
    /// The current page URL — the Level-1 key (after normalization) and the
    /// per-conversation turn-tracking key.
    #[serde(default)]
    pub url: Option<String>,
    /// The extension's guess at the provider. **Ignored** for keying — the daemon
    /// re-derives the provider from the host ([`provider_for_host`]), the only robust
    /// field (§8) — but accepted for forward-compat / debugging.
    #[serde(default)]
    #[allow(dead_code)] // part of the documented capture surface (§6); not keyed on
    pub provider: Option<String>,
    /// The provider's stable, opaque project id (Level-2 key). Absent on an ungrouped
    /// chat.
    #[serde(default)]
    pub provider_project_id: Option<String>,
    /// The provider's readable project name — the source of a minted slug.
    #[serde(default)]
    pub provider_project_name: Option<String>,
    /// The ambient/error discriminator the content-script computes from *presence of a
    /// project-affordance container* (§3): `"grouped"`, `"ambient"`, or `"error"`.
    /// Absent → inferred from whether a project id parsed.
    #[serde(default)]
    pub project_state: Option<String>,
    /// Transient status driver (§7): the model is generating.
    #[serde(default)]
    pub streaming: Option<bool>,
    /// Transient status driver (§7): there is text in the composer / input awaited.
    /// Received for completeness (§6); the engine's `awaiting_input` already covers the
    /// "ball's in your court" state, so it isn't separately acted on yet.
    #[serde(default)]
    #[allow(dead_code)]
    pub input_detected: Option<bool>,
    /// Per-conversation turn counter. An *observed increment* is a you-acted signal
    /// (you sent a turn); see [`event_for`].
    #[serde(default)]
    pub turn_count: Option<u32>,
}

/// The §3 resolution outcome for one payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Host not in the provider allowlist, or no usable URL — not our concern.
    Ignore,
    /// A project anchor was present but unparseable — the provider's DOM changed and
    /// the selectors need a patch (§8). Logged, not attributed.
    Error,
    /// A legitimate ungrouped chat (no provider project). Dropped for v1 (see module
    /// doc); `project = null`.
    Ambient,
    /// Resolved to a Whence slug (via Level-1, Level-2, or a fresh mint).
    Attributed(String),
}

/// Map a host to a provider id, or `None` if the host isn't an allowlisted provider
/// (the §3 `host not in allowlist → END` gate). The provider id is what keys the
/// mapping store, so it must be stable; the **host is the only robust field** (§8),
/// which is why attribution keys on it rather than the extension's self-report.
///
/// This is the centralized provider-by-host allowlist — extend it (and the
/// extension's `providers.js`) to add a surface. Most surfaces are one provider per
/// host; when a single host serves more than one (claude.ai serves chat *and* Claude
/// Design), the path disambiguates in [`provider_for_url`], which is what the call
/// sites use. This stays the host primitive.
pub fn provider_for_host(host: &str) -> Option<&'static str> {
    let h = host.to_lowercase();
    if h == "claude.ai" || h.ends_with(".claude.ai") {
        Some("claude")
    } else if h == "chatgpt.com" || h.ends_with(".chatgpt.com") || h == "chat.openai.com" {
        Some("chatgpt")
    } else {
        None
    }
}

/// Map a (host + path) to a provider id — the path-aware selection (claude-design §1).
/// One host can serve multiple provider surfaces; the path-prefix disambiguates,
/// **specificity-ordered** (§2): the more specific `/design` surface is checked before
/// the host's default surface. The extension's `content.js` must apply the same rule —
/// the "extension and daemon agree on provider derivation" invariant now covers the
/// path, not just the host (§7).
///
/// Today only `claude.ai` is multi-surface; every other host falls straight through to
/// its single [`provider_for_host`] provider regardless of path.
pub fn provider_for_url(host: &str, path: &str) -> Option<&'static str> {
    match provider_for_host(host)? {
        // claude.ai serves chat at the host default and Claude Design under /design.
        // Design is its own provider id (its own store keyspace, §1), not a sub-surface
        // of `claude`; it converges with chat/fs at the slug layer (§5).
        "claude" if path_has_prefix(path, "/design") => Some("claude-design"),
        other => Some(other),
    }
}

/// The path portion of a URL (leading slash kept), or `"/"` when there's none. Pure,
/// same hand-rolled scope as [`host_of`] — the inputs are the handful of provider URLs.
fn path_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    match rest.find('/') {
        Some(i) => {
            let p = &rest[i..];
            p.split(['?', '#']).next().unwrap_or(p).to_string()
        }
        None => "/".to_string(),
    }
}

/// Whether `path` lies under `prefix` as a *path segment* boundary: `prefix` itself or
/// `prefix/…`, but not `prefixfoo` (so `/design` matches `/design` and `/design/x` but
/// never `/designs`). Mirror this in the extension's `pathHasPrefix`.
fn path_has_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || (path.starts_with(prefix) && path[prefix.len()..].starts_with('/'))
}

/// The host portion of a URL, lowercased, sans scheme / userinfo / port / path.
/// `None` if there's nothing host-shaped. Pure — no url crate (the inputs are the
/// handful of provider URLs, not arbitrary user input).
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host_port = rest.split(['/', '?', '#']).next()?;
    let host = host_port.rsplit('@').next()?; // strip any userinfo
    let host = host.split(':').next()?; // strip any port
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// Normalize a URL to its Level-1 key: drop the query and fragment, lowercase the
/// scheme + host (but **not** the path — conversation ids are case-sensitive), and
/// trim a trailing slash. So `https://Claude.ai/project/X/?ref=1#top` and
/// `https://claude.ai/project/X` map to the same key.
pub fn normalize_url(url: &str) -> String {
    let url = url.split('#').next().unwrap_or(url);
    let url = url.split('?').next().unwrap_or(url);
    let (scheme, rest) = url.split_once("://").map(|(s, r)| (s, r)).unwrap_or(("", url));
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, Some(p))).unwrap_or((rest, None));
    let mut out = String::new();
    if !scheme.is_empty() {
        out.push_str(&scheme.to_lowercase());
        out.push_str("://");
    }
    out.push_str(&host.to_lowercase());
    if let Some(p) = path {
        out.push('/');
        out.push_str(p);
    }
    out.trim_end_matches('/').to_string()
}

/// Run the §3 resolution for a payload against the mapping store, performing the
/// Level-2 backfill / mint as needed. Provider keying uses the **host-derived**
/// provider, not the extension's self-report.
pub fn resolve(p: &BrowserPayload, store: &mut MappingStore) -> Resolution {
    let Some(url) = p.url.as_deref() else { return Resolution::Ignore };
    let Some(host) = host_of(url) else { return Resolution::Ignore };
    let Some(provider) = provider_for_url(&host, &path_of(url)) else { return Resolution::Ignore };

    let norm = normalize_url(url);

    // Level-1: the URL is already mapped — fast path, no DOM read needed.
    if let Some(slug) = store.lookup_url(&norm) {
        return Resolution::Attributed(slug);
    }

    // Fall to the DOM-read project. The discriminator is best-effort (§3/§8).
    match p.project_state.as_deref() {
        Some("error") => return Resolution::Error,
        Some("ambient") => return Resolution::Ambient,
        _ => {} // "grouped" or unspecified → decide on the parsed id below
    }

    // The stable id is the Level-2 key. Without one there's nothing to look up or mint
    // → a legitimate ungrouped chat.
    let id = match p.provider_project_id.as_deref() {
        Some(id) if !id.is_empty() => id,
        _ => return Resolution::Ambient,
    };

    // Level-2: id already known → attribute and cache this URL for next time. Needs
    // only the id, so a known project still resolves even when the readable name
    // didn't parse on this load (DOM churn, slow render, …).
    if let Some(slug) = store.lookup_provider(provider, id) {
        store.backfill_url(provider, id, &norm);
        return Resolution::Attributed(slug);
    }

    // First time we've seen this id → mint from the readable name (slugify) and cache
    // the URL. Minting *requires* a name; a known id without one would have resolved
    // above, so a missing name here means a brand-new project we can't name yet —
    // ambient until the name parses (then it mints).
    let name = match p.provider_project_name.as_deref() {
        Some(name) if !name.trim().is_empty() => name,
        _ => return Resolution::Ambient,
    };
    match store.mint(provider, id, name) {
        Some(slug) => {
            store.backfill_url(provider, id, &norm);
            Resolution::Attributed(slug)
        }
        // Name slugged to nothing — treat as ambient rather than inventing a slug.
        None => Resolution::Ambient,
    }
}

/// Build the `WorkEvent` for an attributed browser session. **Pure** — the clock and
/// the per-conversation `prev_turn` come in as parameters.
///
/// The transient booleans are *status drivers*, not persisted timeline events (§7).
/// The mapping into the engine's kind:
///   * **observed turn increment** → [`WorkKind::Prompt`] — you sent a turn, so this
///     is a *you-acted* signal: it switches the active project immediately and marks
///     it *present*, exactly like a Claude Code `UserPromptSubmit`. A first sighting
///     (no `prev_turn`) is **not** counted as an act — merely having the tab open must
///     not hijack focus; it takes a real increment.
///   * else **streaming** → [`WorkKind::Active`] — the model is generating: autonomous
///     activity, which goes through the normal switch debounce.
///   * else → [`WorkKind::AwaitingInput`] — the turn is done / the chat is idle: a
///     status-only signal (the browser equivalent of Claude Code's `Stop`).
pub fn event_for(
    slug: String,
    p: &BrowserPayload,
    prev_turn: Option<u32>,
    now_rfc3339: &str,
) -> WorkEvent {
    let new_turn = matches!((p.turn_count, prev_turn), (Some(n), Some(prev)) if n > prev);
    let kind = if new_turn {
        WorkKind::Prompt
    } else if p.streaming.unwrap_or(false) {
        WorkKind::Active
    } else {
        WorkKind::AwaitingInput
    };
    let (source, source_label) = match p.url.as_deref() {
        // The conversation URL is the per-session source key (one row per chat); the
        // host's provider gives the human label ("claude web" / "chatgpt web") that the
        // bare `Browser` surface can't carry.
        Some(url) => (
            Some(normalize_url(url)),
            host_of(url).and_then(|h| provider_for_url(&h, &path_of(url))).map(source_label_for),
        ),
        None => (None, None),
    };
    WorkEvent {
        ts: now_rfc3339.to_string(),
        surface: Surface::Browser,
        project: Some(slug),
        source,
        source_label,
        kind,
        confidence: 1.0, // self-attributed from the provider's own project identity (§2)
        detail: p.url.clone().map(|u| format!("browser: {u}")),
    }
}

/// Map a provider id to its source-row label.
fn source_label_for(provider: &str) -> String {
    match provider {
        "claude" => "claude web".to_string(),
        // Distinct row label so the tree shows `claude web` and `claude design` as
        // separate children under one project node (§1). Not "claude-design web".
        "claude-design" => "claude design".to_string(),
        "chatgpt" => "chatgpt web".to_string(),
        other => format!("{other} web"),
    }
}

/// Resolve + derive an event for one payload, updating the turn-count memory. Returns
/// `None` for the non-attributed outcomes (ignore / ambient / error). `now_rfc3339`
/// and `last_turn` are passed so this stays testable without a clock or globals.
fn handle(
    p: &BrowserPayload,
    store: &mut MappingStore,
    last_turn: &mut HashMap<String, u32>,
    now_rfc3339: &str,
) -> Option<WorkEvent> {
    match resolve(p, store) {
        Resolution::Attributed(slug) => {
            let key = p.url.as_deref().map(normalize_url);
            let prev = key.as_deref().and_then(|k| last_turn.get(k).copied());
            let ev = event_for(slug, p, prev, now_rfc3339);
            if let (Some(k), Some(n)) = (key, p.turn_count) {
                last_turn.insert(k, n);
            }
            Some(ev)
        }
        Resolution::Error => {
            eprintln!(
                "whence: browser extraction error (provider DOM changed?) for {:?}",
                p.url
            );
            None
        }
        Resolution::Ambient | Resolution::Ignore => None,
    }
}

/// Bind the loopback browser-receiver endpoint and serve forever on a dedicated
/// thread, emitting `WorkEvent`s on `tx`. Returns once the socket is bound (an error
/// means the bind failed — e.g. the port is taken); the serving loop then runs for
/// the process lifetime. Mirrors [`super::hooks::serve`]: accept fast, never block,
/// drop malformed payloads rather than rejecting them.
///
/// **Bearer-gated** (`crate::auth`), and strictly so: this receiver is
/// *originating-capable* — a forged payload could mint projects into
/// `browser_mapping.toml` and re-attribute focus wholesale — and `GET /raise`
/// would otherwise hand queued conversation URLs to any local poller. The
/// extension sends the token (pasted once into its options page) as an
/// `Authorization` header on both. The token is read per request from the shared
/// handle so a Settings rotation applies live.
///
/// The thread owns the mutable [`MappingStore`] (single-threaded, so no lock) and the
/// per-conversation turn-count memory.
pub fn serve(
    tx: UnboundedSender<WorkEvent>,
    mut store: MappingStore,
    addr: &str,
    raise_q: RaiseQueue,
    token: crate::auth::SharedToken,
    denials: crate::auth::Denials,
) -> Result<(), String> {
    let server = tiny_http::Server::http(addr)
        .map_err(|e| format!("could not bind browser receiver on {addr}: {e}"))?;

    std::thread::Builder::new()
        .name("whence-browser".into())
        .spawn(move || {
            let mut last_turn: HashMap<String, u32> = HashMap::new();
            for mut req in server.incoming_requests() {
                // Auth inside a scope: the read guard (poison-recovering, no clone)
                // drops before the body read — mirrors the hooks receiver.
                {
                    let tok = crate::auth::read_token(&token);
                    if !crate::auth::tiny_http_authorized(&req, &tok) {
                        crate::auth::record_denial(&denials, "browser");
                        let _ = req.respond(tiny_http::Response::empty(401));
                        continue;
                    }
                }
                // Back-channel: the extension polls here for tabs the widget asked to
                // raise. Drain-on-read — each target is delivered once, so a raise
                // never re-fires on the next poll. No CORS header needed: the poll
                // fetch is to a `host_permissions` origin (127.0.0.1), same as the POST.
                if req.method() == &tiny_http::Method::Get && req.url().starts_with("/raise") {
                    let urls: Vec<String> = raise_q
                        .lock()
                        .map(|mut q| q.drain(..).collect())
                        .unwrap_or_default();
                    let body = serde_json::json!({ "raise": urls }).to_string();
                    let hdr = tiny_http::Header::from_bytes(
                        &b"Content-Type"[..],
                        &b"application/json"[..],
                    )
                    .expect("static header is valid");
                    let _ = req.respond(tiny_http::Response::from_string(body).with_header(hdr));
                    continue;
                }

                let mut body = String::new();
                if req.as_reader().read_to_string(&mut body).is_ok() {
                    if let Ok(payload) = serde_json::from_str::<BrowserPayload>(&body) {
                        let now = chrono::Utc::now().to_rfc3339();
                        if let Some(ev) = handle(&payload, &mut store, &mut last_turn, &now) {
                            // Unbounded send never blocks; an error only means the core
                            // task is gone (shutdown), so dropping is correct.
                            let _ = tx.send(ev);
                        }
                    }
                }
                let _ = req.respond(tiny_http::Response::empty(200));
            }
        })
        .map_err(|e| format!("could not spawn browser receiver thread: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raise_queue_dedupes_and_drains_once() {
        let q: RaiseQueue = Arc::new(Mutex::new(VecDeque::new()));
        enqueue_raise(&q, "https://claude.ai/project/a".into());
        enqueue_raise(&q, "https://claude.ai/project/b".into());
        // A repeat moves to the back rather than queueing twice (newest click wins).
        enqueue_raise(&q, "https://claude.ai/project/a".into());

        // Drain-on-read: every pending target comes out once, in order...
        let drained: Vec<String> = q.lock().unwrap().drain(..).collect();
        assert_eq!(
            drained,
            vec![
                "https://claude.ai/project/b".to_string(),
                "https://claude.ai/project/a".to_string(),
            ]
        );
        // ...and a second drain is empty — a raise never re-fires on the next poll.
        assert!(q.lock().unwrap().drain(..).next().is_none());
    }

    const NOW: &str = "2026-06-26T12:00:00Z";

    const SEED: &str = r#"
[[project]]
slug                  = "whence"
provider              = "claude"
provider_project_id   = "proj_abc"
provider_project_name = "Whence"
urls                  = ["https://claude.ai/project/proj_abc"]
"#;

    fn payload() -> BrowserPayload {
        BrowserPayload::default()
    }

    #[test]
    fn provider_allowlist() {
        assert_eq!(provider_for_host("claude.ai"), Some("claude"));
        assert_eq!(provider_for_host("www.claude.ai"), Some("claude"));
        assert_eq!(provider_for_host("chatgpt.com"), Some("chatgpt"));
        assert_eq!(provider_for_host("chat.openai.com"), Some("chatgpt"));
        // Not a provider host → not our concern.
        assert_eq!(provider_for_host("example.com"), None);
        assert_eq!(provider_for_host("notclaude.ai"), None); // suffix guard
    }

    #[test]
    fn provider_for_url_is_path_aware_for_claude_design() {
        // claude.ai default surface (chat) vs. the /design surface (§1/§2).
        assert_eq!(provider_for_url("claude.ai", "/project/x"), Some("claude"));
        assert_eq!(provider_for_url("claude.ai", "/design"), Some("claude-design"));
        assert_eq!(provider_for_url("claude.ai", "/design/abc"), Some("claude-design"));
        // Specificity is segment-bounded: /designs is NOT the design surface.
        assert_eq!(provider_for_url("claude.ai", "/designs"), Some("claude"));
        assert_eq!(provider_for_url("claude.ai", "/designer/x"), Some("claude"));
        // Host default with no/empty path is still chat.
        assert_eq!(provider_for_url("claude.ai", "/"), Some("claude"));
        // Other hosts fall straight through, path-independent.
        assert_eq!(provider_for_url("chatgpt.com", "/design/x"), Some("chatgpt"));
        assert_eq!(provider_for_url("example.com", "/design"), None);
    }

    #[test]
    fn path_of_extracts_path_sans_query_fragment() {
        assert_eq!(path_of("https://claude.ai/design/x?ref=1#top"), "/design/x");
        assert_eq!(path_of("https://claude.ai"), "/");
        assert_eq!(path_of("claude.ai/design"), "/design"); // scheme-less
        assert_eq!(path_of("https://claude.ai/?q=1"), "/");
    }

    #[test]
    fn host_and_url_normalization() {
        assert_eq!(host_of("https://claude.ai/project/x?q=1#frag").as_deref(), Some("claude.ai"));
        assert_eq!(host_of("https://chat.openai.com:443/c/1").as_deref(), Some("chat.openai.com"));
        // Query, fragment, trailing slash, and host-case all normalize away; path case kept.
        assert_eq!(
            normalize_url("https://Claude.ai/project/AbC/?ref=1#top"),
            "https://claude.ai/project/AbC"
        );
        assert_eq!(normalize_url("https://claude.ai/project/AbC"), "https://claude.ai/project/AbC");
    }

    #[test]
    fn non_provider_host_is_ignored() {
        let mut store = MappingStore::from_str("").unwrap();
        let mut p = payload();
        p.url = Some("https://example.com/chat".into());
        assert_eq!(resolve(&p, &mut store), Resolution::Ignore);
        // No URL at all is also ignored.
        assert_eq!(resolve(&payload(), &mut store), Resolution::Ignore);
    }

    #[test]
    fn level1_url_hits_without_dom() {
        let mut store = MappingStore::from_str(SEED).unwrap();
        let mut p = payload();
        // A conversation under the project — the project URL is on record, but even a
        // bare project URL hit needs no DOM read.
        p.url = Some("https://claude.ai/project/proj_abc?foo=bar".into());
        assert_eq!(resolve(&p, &mut store), Resolution::Attributed("whence".into()));
    }

    #[test]
    fn explicit_ambient_and_error_states() {
        let mut store = MappingStore::from_str("").unwrap();
        let mut amb = payload();
        amb.url = Some("https://claude.ai/new".into());
        amb.project_state = Some("ambient".into());
        assert_eq!(resolve(&amb, &mut store), Resolution::Ambient);

        let mut err = payload();
        err.url = Some("https://claude.ai/chat/123".into());
        err.project_state = Some("error".into());
        assert_eq!(resolve(&err, &mut store), Resolution::Error);
    }

    #[test]
    fn no_project_anchor_is_ambient() {
        let mut store = MappingStore::from_str("").unwrap();
        let mut p = payload();
        p.url = Some("https://chatgpt.com/c/abc".into());
        // grouped state claimed but nothing parsed → still ambient, not error.
        p.project_state = Some("grouped".into());
        assert_eq!(resolve(&p, &mut store), Resolution::Ambient);
    }

    #[test]
    fn level2_match_attributes_and_backfills_url() {
        let mut store = MappingStore::from_str(SEED).unwrap();
        let mut p = payload();
        // A *new* conversation URL under a known project id — L1 misses, L2 hits.
        p.url = Some("https://claude.ai/project/proj_abc/conversation/new1".into());
        p.provider_project_id = Some("proj_abc".into());
        p.provider_project_name = Some("Whence".into());
        assert_eq!(resolve(&p, &mut store), Resolution::Attributed("whence".into()));
        // The URL is now backfilled → a second visit takes the L1 fast path.
        assert_eq!(
            store.lookup_url("https://claude.ai/project/proj_abc/conversation/new1").as_deref(),
            Some("whence")
        );
    }

    #[test]
    fn known_id_resolves_even_without_a_parsed_name() {
        // The name didn't render this load, but the id is on record → L2 still attributes
        // (only mint needs a name). Without this, a momentary DOM hiccup would drop a
        // known project to ambient.
        let mut store = MappingStore::from_str(SEED).unwrap();
        let mut p = payload();
        p.url = Some("https://claude.ai/chat/conv-xyz".into());
        p.provider_project_id = Some("proj_abc".into());
        p.provider_project_name = None;
        p.project_state = Some("grouped".into());
        assert_eq!(resolve(&p, &mut store), Resolution::Attributed("whence".into()));
    }

    #[test]
    fn brand_new_id_without_a_name_is_ambient_until_named() {
        // An unknown id with no name yet can't mint — ambient until the name parses.
        let mut store = MappingStore::from_str("").unwrap();
        let mut p = payload();
        p.url = Some("https://claude.ai/chat/conv-1".into());
        p.provider_project_id = Some("brand-new".into());
        p.provider_project_name = None;
        assert_eq!(resolve(&p, &mut store), Resolution::Ambient);
    }

    #[test]
    fn unknown_project_mints_a_slug_and_caches() {
        let mut store = MappingStore::from_str("").unwrap();
        let mut p = payload();
        p.url = Some("https://chatgpt.com/g/g-xyz/project".into());
        p.provider_project_id = Some("g-xyz".into());
        p.provider_project_name = Some("One Domino Square".into());
        // Minted via the shared slugify → converges with the fs slug for the same name.
        assert_eq!(resolve(&p, &mut store), Resolution::Attributed("one-domino-square".into()));
        // Registered under the provider id, and the URL is cached.
        assert_eq!(store.lookup_provider("chatgpt", "g-xyz").as_deref(), Some("one-domino-square"));
        assert_eq!(
            store.lookup_url("https://chatgpt.com/g/g-xyz/project").as_deref(),
            Some("one-domino-square")
        );
    }

    #[test]
    fn design_mints_under_its_own_provider_keyspace() {
        // A /design session resolves to provider `claude-design`, a separate keyspace
        // from chat's `claude` — no collision even if the ids ever overlapped (§1).
        let mut store = MappingStore::from_str("").unwrap();
        let mut p = payload();
        p.url = Some("https://claude.ai/design/d-123".into());
        p.provider_project_id = Some("d-123".into());
        p.provider_project_name = Some("Whence".into());
        // Minted via the shared slugify → converges with the fs `whence` at the slug
        // layer (§5), while keyed independently of any chat project.
        assert_eq!(resolve(&p, &mut store), Resolution::Attributed("whence".into()));
        assert_eq!(store.lookup_provider("claude-design", "d-123").as_deref(), Some("whence"));
        // The chat keyspace is untouched: the same id under `claude` is unknown.
        assert!(store.lookup_provider("claude", "d-123").is_none());
    }

    #[test]
    fn event_streaming_is_autonomous_active() {
        let mut p = payload();
        p.url = Some("https://claude.ai/project/x".into());
        p.streaming = Some(true);
        let ev = event_for("whence".into(), &p, None, NOW);
        assert_eq!(ev.surface, Surface::Browser);
        assert_eq!(ev.kind, WorkKind::Active);
        assert_eq!(ev.project.as_deref(), Some("whence"));
        assert_eq!(ev.confidence, 1.0);
        assert_eq!(ev.ts, NOW);
        assert_eq!(ev.detail.as_deref(), Some("browser: https://claude.ai/project/x"));
    }

    #[test]
    fn event_carries_conversation_source_and_provider_label() {
        let mut p = payload();
        p.url = Some("https://chatgpt.com/c/abc?ref=1".into());
        p.streaming = Some(true);
        let ev = event_for("whence".into(), &p, None, NOW);
        // The normalized conversation URL is the per-session source key; the host gives
        // the human label the bare `Browser` surface can't carry.
        assert_eq!(ev.source.as_deref(), Some("https://chatgpt.com/c/abc"));
        assert_eq!(ev.source_label.as_deref(), Some("chatgpt web"));

        let mut claude = payload();
        claude.url = Some("https://claude.ai/project/x".into());
        let ev = event_for("whence".into(), &claude, None, NOW);
        assert_eq!(ev.source_label.as_deref(), Some("claude web"));

        // A /design URL earns the distinct "claude design" row label (§1).
        let mut design = payload();
        design.url = Some("https://claude.ai/design/d-1".into());
        let ev = event_for("whence".into(), &design, None, NOW);
        assert_eq!(ev.source_label.as_deref(), Some("claude design"));
    }

    #[test]
    fn event_turn_increment_is_you_acted_prompt() {
        let mut p = payload();
        p.turn_count = Some(3);
        // We saw turn 2 before → an increment → you sent a turn → Prompt (immediate switch).
        let ev = event_for("whence".into(), &p, Some(2), NOW);
        assert_eq!(ev.kind, WorkKind::Prompt);
        assert!(ev.kind.is_focus_evidence());
    }

    #[test]
    fn event_first_sighting_does_not_hijack_focus() {
        let mut p = payload();
        // Tab just opened on an existing conversation (turn 5) — no prior count. Merely
        // having it open is not an act, so it's not a Prompt.
        p.turn_count = Some(5);
        let ev = event_for("whence".into(), &p, None, NOW);
        assert_ne!(ev.kind, WorkKind::Prompt);
        // Not streaming, not a new turn → status-only awaiting.
        assert_eq!(ev.kind, WorkKind::AwaitingInput);
    }

    #[test]
    fn event_idle_session_is_awaiting() {
        let p = payload(); // no streaming, no turn
        let ev = event_for("whence".into(), &p, None, NOW);
        assert_eq!(ev.kind, WorkKind::AwaitingInput);
        assert!(!ev.kind.is_focus_evidence());
    }

    #[test]
    fn handle_tracks_turns_across_pings() {
        let mut store = MappingStore::from_str(SEED).unwrap();
        let mut last_turn = HashMap::new();
        let url = "https://claude.ai/project/proj_abc/conversation/c1";

        // First ping: turn 1, not streaming → first sighting, awaiting (no hijack).
        let mut p = payload();
        p.url = Some(url.into());
        p.provider_project_id = Some("proj_abc".into());
        p.provider_project_name = Some("Whence".into());
        p.turn_count = Some(1);
        let e1 = handle(&p, &mut store, &mut last_turn, NOW).unwrap();
        assert_eq!(e1.kind, WorkKind::AwaitingInput);

        // Second ping: turn 2 → observed increment → you-acted Prompt.
        p.turn_count = Some(2);
        let e2 = handle(&p, &mut store, &mut last_turn, NOW).unwrap();
        assert_eq!(e2.kind, WorkKind::Prompt);

        // Ambient / non-provider pings produce no event.
        let mut amb = payload();
        amb.url = Some("https://example.com".into());
        assert!(handle(&amb, &mut store, &mut last_turn, NOW).is_none());
    }

    #[test]
    fn payload_tolerates_unknown_and_missing_fields() {
        let json = r#"{"url":"https://claude.ai/new","streaming":true,"future_field":42}"#;
        let p: BrowserPayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.url.as_deref(), Some("https://claude.ai/new"));
        assert_eq!(p.streaming, Some(true));
    }
}
