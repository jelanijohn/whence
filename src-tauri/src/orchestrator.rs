//! Orchestration: wire adapters → segmenter → outputs. This is the only place
//! that's allowed to be impure around the pure engine — it reads the clock, does
//! I/O (timeline append), fires the NeuroSkill label, and pushes snapshots to the
//! widget. The decision logic all lives in `engine::segment`.
//!
//! Context strings (docs/context-strings.md) are attached *here*, downstream of
//! segmentation — the engine neither receives nor emits them (spec §2), which is
//! why the widget/timeline shapes below wrap the engine types instead of the
//! engine types growing fields.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::adapters::WorkEvent;
use crate::context::{self, ContextSource, ContextString, SharedMoments, SharedRoots, StoredContext};
use crate::engine::segment::{Effect, FocusSnapshot, Segmenter};
use crate::engine::timeline;
use crate::neuroskill;
use crate::settings::Settings;

/// The event Whence emits to the widget on every focus/status change.
pub const FOCUS_EVENT: &str = "whence://focus";

/// What the widget actually receives: the engine snapshot plus the display-only
/// context string for the *focused* project, if one resolved. Flattened, so the
/// wire shape is the old `FocusSnapshot` with an optional `context` key — the
/// engine type itself stays context-free (spec §2 tripwire).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetSnapshot {
    #[serde(flatten)]
    pub snapshot: FocusSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextString>,
}

impl Default for WidgetSnapshot {
    fn default() -> Self {
        Self {
            snapshot: FocusSnapshot { projects: Vec::new() },
            context: None,
        }
    }
}

/// The persisted timeline line: the engine block plus the optional context stamp
/// applied at block close (spec §5). Flattened — existing JSONL lines (no
/// `context` key) still deserialize; readers tolerate absence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineRecord {
    #[serde(flatten)]
    pub block: crate::engine::segment::FocusBlock,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<StoredContext>,
}

impl timeline::HasStart for TimelineRecord {
    fn start_secs(&self) -> i64 {
        self.block.start
    }
}

/// Shared, command-readable current snapshot. The core task is the sole writer.
pub type SharedSnapshot = Arc<Mutex<WidgetSnapshot>>;

/// A completed context resolution: the root it ran for, and what it found.
type ResolvedContext = (PathBuf, Option<ContextString>);

/// Run the core loop: consume `WorkEvent`s, drive the segmenter, and fan the
/// effects out to the timeline, NeuroSkill, and the widget. Runs until `rx`
/// closes (app shutdown). `now_secs` is injected so tests can drive it; in
/// production it's the wall clock.
///
/// `settings` is the live shared handle — the NeuroSkill and context toggles
/// apply without a restart; the segmenter *config* is still fixed at startup
/// (the engine reads it once, as before).
pub async fn run(
    app: AppHandle,
    mut rx: UnboundedReceiver<WorkEvent>,
    shared: SharedSnapshot,
    data_dir: std::path::PathBuf,
    settings: Arc<Mutex<Settings>>,
    roots: SharedRoots,
    // Per-project *moments* (docs/context-strings.md §10) — prompt snippets and
    // conversation titles the capture-enabled receivers record. This loop only
    // arbitrates (per-source gates in `focus_context`) and clears on block close;
    // in-memory and display-only throughout (the A2 raw-capture posture).
    moments: SharedMoments,
) {
    let mut seg = Segmenter::new(read_settings(&settings).segment_config());
    let timeline_path = timeline::timeline_path(&data_dir);
    let mut idle_check = tokio::time::interval(std::time::Duration::from_secs(30));
    let mut ctx_cache = context::ContextCache::new();
    // Completed context resolutions land here; the loop folds them into the cache
    // and re-pushes the snapshot (the spec's "follow-up emit", §4). Holding this
    // sender for the loop's lifetime keeps the branch alive.
    let (ctx_tx, mut ctx_rx) = tokio::sync::mpsc::unbounded_channel::<ResolvedContext>();

    loop {
        let effects = tokio::select! {
            maybe = rx.recv() => match maybe {
                Some(ev) => seg.ingest(&ev),
                None => break, // channel closed → shutting down
            },
            _ = idle_check.tick() => seg.tick(now_secs()),
            Some((root, ctx)) = ctx_rx.recv() => {
                // A resolution finished: cache it (freshness dates from completion)
                // and push a snapshot so the widget picks it up. Reusing
                // SnapshotDirty keeps the effect path uniform.
                ctx_cache.store(&root, ctx, now_secs());
                vec![Effect::SnapshotDirty]
            }
        };

        if effects.is_empty() {
            continue;
        }

        let cfg = read_settings(&settings);
        for effect in &effects {
            match effect {
                Effect::BlockOpened { project, .. } => {
                    if cfg.neuroskill_enabled {
                        fire_label(&cfg, neuroskill::start_label(project));
                    }
                }
                Effect::BlockClosed(block) => {
                    // Stamp the closing block with the last resolved *git* context
                    // for its project (any age — recall beats freshness here, §5).
                    // Hook-derived moments are deliberately not stamped: display-
                    // only, per the A2 raw-capture decision.
                    let record = TimelineRecord {
                        block: block.clone(),
                        context: closing_context(&cfg, &roots, &ctx_cache, &block.project),
                    };
                    if let Err(e) = timeline::append_block(&timeline_path, &record) {
                        eprintln!("whence: timeline append failed: {e}");
                    }
                    // The moment described this block; a fresh block starts on the
                    // git fallback until you prompt again.
                    context::clear_moment(&moments, &block.project);
                    if cfg.neuroskill_enabled {
                        fire_label(&cfg, neuroskill::end_label(&block.project));
                    }
                }
                Effect::SnapshotDirty => { /* snapshot push below covers it */ }
            }
        }

        // The engine snapshot is authoritative for the widget; the context string
        // rides alongside, never delaying it — a cache miss emits without context
        // and the async resolve triggers the follow-up push above.
        let snap = seg.snapshot();
        let ctx = focus_context(&cfg, &roots, &mut ctx_cache, &moments, &ctx_tx, &snap);
        let widget = WidgetSnapshot { snapshot: snap, context: ctx };
        if let Ok(mut guard) = shared.lock() {
            *guard = widget.clone();
        }
        let _ = app.emit(FOCUS_EVENT, widget);
    }
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Snapshot the live settings; a poisoned lock falls back to defaults rather than
/// panicking the core task.
fn read_settings(settings: &Arc<Mutex<Settings>>) -> Settings {
    settings.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Is a moment from this source currently displayable? Each content-derived
/// source is gated by its own live toggle (docs/context-strings.md §10) — turning
/// one off hides its stored moments immediately, without touching the others.
fn moment_source_enabled(cfg: &Settings, source: ContextSource) -> bool {
    match source {
        ContextSource::HookPrompt => cfg.context_hook_prompts,
        ContextSource::BrowserTitle => cfg.context_browser_titles,
        // Git never lands in the moments map (it's the per-root fallback tier);
        // defensively inert if it ever did.
        ContextSource::Git => false,
    }
}

/// The context string to attach to this snapshot emit, on the §10 priority
/// ladder: the live *moment* (your prompt snippet / session summary / the
/// conversation title — whichever a receiver recorded last) first, then the
/// per-root git fallback — the cached value (stale is fine — better a last-known
/// line than a flash of nothing), kicking off an async re-resolve when the cache
/// is missing or past TTL. Every `None` on the way — toggle off, no focus,
/// rootless project — is silent degradation (spec §2.3), never an error.
fn focus_context(
    cfg: &Settings,
    roots: &SharedRoots,
    cache: &mut context::ContextCache,
    moments: &SharedMoments,
    ctx_tx: &UnboundedSender<ResolvedContext>,
    snap: &FocusSnapshot,
) -> Option<ContextString> {
    if !cfg.context_strings {
        return None;
    }
    let slug = snap.projects.iter().find(|p| p.active).map(|p| p.project.as_str())?;
    // A moment needs no root — a prompt or title gives context even where git can't.
    if let Some(moment) = context::moment_for(moments, slug) {
        if moment_source_enabled(cfg, moment.source) {
            return Some(moment);
        }
    }
    let root = context::root_for(roots, slug)?;
    let now = now_secs();
    let (cached, needs_resolve) = match cache.get(&root) {
        Some((ctx, fetched_at)) => {
            (ctx.clone(), now - fetched_at >= cfg.context_ttl_seconds as i64)
        }
        None => (None, true),
    };
    if needs_resolve && cache.begin(&root) {
        let tx = ctx_tx.clone();
        tokio::spawn(async move {
            let ctx = context::resolve(&root, now_secs()).await;
            // Send failure = the core loop is gone (shutdown); nothing to do.
            let _ = tx.send((root, ctx));
        });
    }
    cached
}

/// The stamp for a closing block: the last resolved context for its project,
/// regardless of age — what makes past timeline rows recallable (spec §5).
fn closing_context(
    cfg: &Settings,
    roots: &SharedRoots,
    cache: &context::ContextCache,
    project: &str,
) -> Option<StoredContext> {
    if !cfg.context_strings {
        return None;
    }
    let root = context::root_for(roots, project)?;
    cache.last(&root).map(|c| StoredContext::from(&c))
}

/// Fire one NeuroSkill label, best-effort, off the hot path. A failure (daemon
/// down, wrong token) is logged and never blocks focus tracking. The endpoint and
/// token path honor settings overrides; the token is resolved at call time so a
/// rotated token (and WSL2 Windows-host discovery) is picked up without restart.
fn fire_label(settings: &Settings, text: String) {
    let base = settings
        .neuroskill_endpoint
        .clone()
        .unwrap_or_else(|| neuroskill::DEFAULT_ENDPOINT.to_string());
    let token = neuroskill::client::load_token(settings.neuroskill_token_path.as_deref());
    tokio::spawn(async move {
        if let Err(e) = neuroskill::client::fire_label(&base, token.as_deref(), &text).await {
            eprintln!("whence: NeuroSkill label '{text}' not written: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::segment::FocusBlock;

    // Serde tolerance (docs/context-strings.md §9): the additive JSONL/wire
    // change must leave pre-existing lines readable and keep the old shape when
    // no context resolved.

    #[test]
    fn legacy_timeline_line_still_deserializes() {
        let line = r#"{"project":"waid","start":100,"end":200,"eventCount":3,"meanConfidence":0.9}"#;
        let rec: TimelineRecord = serde_json::from_str(line).unwrap();
        assert!(rec.context.is_none());
        assert_eq!(rec.block.project, "waid");
        assert_eq!(rec.block.event_count, 3);
    }

    #[test]
    fn stamped_record_flattens_and_roundtrips() {
        let rec = TimelineRecord {
            block: FocusBlock {
                project: "whence".into(),
                start: 100,
                end: 200,
                event_count: 3,
                mean_confidence: 0.9,
            },
            context: Some(StoredContext {
                text: "main · add HEAD parser".into(),
                source: crate::context::ContextSource::Git,
            }),
        };
        let json = serde_json::to_string(&rec).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Flattened: block fields at top level, context beside them.
        assert_eq!(val["project"], "whence");
        assert_eq!(val["context"]["text"], "main · add HEAD parser");
        assert_eq!(val["context"]["source"], "git");
        let back: TimelineRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.context, rec.context);
    }

    #[test]
    fn contextless_record_serializes_to_the_old_shape() {
        let rec = TimelineRecord {
            block: FocusBlock {
                project: "waid".into(),
                start: 1,
                end: 2,
                event_count: 1,
                mean_confidence: 1.0,
            },
            context: None,
        };
        let json = serde_json::to_string(&rec).unwrap();
        // No `context` key at all — byte-compatible with pre-feature lines.
        assert!(!json.contains("context"));
        assert_eq!(json, serde_json::to_string(&rec.block).unwrap());
    }

    #[test]
    fn legacy_snapshot_payload_still_deserializes() {
        let json = r#"{"projects":[]}"#;
        let snap: WidgetSnapshot = serde_json::from_str(json).unwrap();
        assert!(snap.context.is_none());
        assert!(snap.snapshot.projects.is_empty());
    }

    // --- Moments (docs/context-strings.md §10) ---------------------------------

    /// A snapshot with `slug` as the single, active project.
    fn active_snap(slug: &str) -> FocusSnapshot {
        use crate::engine::segment::{ProjectSnapshot, Status};
        FocusSnapshot {
            projects: vec![ProjectSnapshot {
                project: slug.to_string(),
                status: Status::Active,
                status_since: Some(0),
                active: true,
                presence: None,
                sources: Vec::new(),
            }],
        }
    }

    #[test]
    fn moment_outranks_git_and_needs_no_root() {
        let mut cfg = Settings::default();
        cfg.context_hook_prompts = true;
        let roots = context::new_roots(); // deliberately empty: no git root at all
        let mut cache = context::ContextCache::new();
        let moments = context::new_moments();
        context::record_moment(&moments, "whence", "fix parser", ContextSource::HookPrompt, 10);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        // §10 ladder: the per-moment string wins — even for a rootless project.
        let ctx =
            focus_context(&cfg, &roots, &mut cache, &moments, &tx, &active_snap("whence"));
        assert_eq!(ctx.as_ref().map(|c| c.text.as_str()), Some("fix parser"));
        assert_eq!(ctx.unwrap().source, ContextSource::HookPrompt);

        // Source gate off → the stored moment is not shown (git fallback, here none).
        cfg.context_hook_prompts = false;
        let ctx =
            focus_context(&cfg, &roots, &mut cache, &moments, &tx, &active_snap("whence"));
        assert!(ctx.is_none());

        // Master toggle off → nothing, regardless of the source gate.
        cfg.context_strings = false;
        cfg.context_hook_prompts = true;
        let ctx =
            focus_context(&cfg, &roots, &mut cache, &moments, &tx, &active_snap("whence"));
        assert!(ctx.is_none());
    }

    #[test]
    fn moment_source_gates_are_independent() {
        // A browser-title moment obeys the title gate, not the prompt gate: with
        // titles on and prompts off, a BrowserTitle moment shows; flip the gates
        // and the same moment hides.
        let mut cfg = Settings::default();
        cfg.context_browser_titles = true;
        cfg.context_hook_prompts = false;
        let roots = context::new_roots();
        let mut cache = context::ContextCache::new();
        let moments = context::new_moments();
        context::record_moment(
            &moments,
            "whence",
            "Debugging the HEAD parser",
            ContextSource::BrowserTitle,
            10,
        );
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let ctx =
            focus_context(&cfg, &roots, &mut cache, &moments, &tx, &active_snap("whence"));
        assert_eq!(ctx.unwrap().source, ContextSource::BrowserTitle);

        cfg.context_browser_titles = false;
        cfg.context_hook_prompts = true;
        let ctx =
            focus_context(&cfg, &roots, &mut cache, &moments, &tx, &active_snap("whence"));
        assert!(ctx.is_none(), "the title gate governs a title moment");
    }

    #[test]
    fn moments_are_display_only_never_stamped() {
        // The A2 raw-capture decision: a content-derived moment shows live but the
        // block stamp comes from git alone — `closing_context` doesn't even see
        // the moments map, and with no git root the stamp is simply absent.
        let mut cfg = Settings::default();
        cfg.context_hook_prompts = true;
        cfg.context_browser_titles = true;
        let roots = context::new_roots();
        let mut cache = context::ContextCache::new();
        let moments = context::new_moments();
        context::record_moment(&moments, "whence", "secret prompt", ContextSource::HookPrompt, 10);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let shown =
            focus_context(&cfg, &roots, &mut cache, &moments, &tx, &active_snap("whence"));
        assert!(shown.is_some(), "the moment displays live");
        assert!(
            closing_context(&cfg, &roots, &cache, "whence").is_none(),
            "but nothing content-derived may reach the persisted block"
        );
    }
}
