//! Context strings (docs/context-strings.md) — a short, human-readable line shown
//! beside the attribution ("feat/context-strings — add HEAD parser fixtures") to
//! jog memory about *what* you were doing there. **Display only.**
//!
//! The load-bearing invariants (spec §2), pinned by the tripwire tests below:
//!
//! 1. **Never in labels.** The NeuroSkill label stays exactly
//!    `Whence:project=<slug>:start|end` — no context, ever.
//! 2. **Never an attribution input.** The engine neither receives nor emits
//!    context; the orchestrator attaches it downstream of segmentation, which is
//!    why the snapshot/record wrappers live in `orchestrator.rs` and nothing under
//!    `engine/` or `neuroskill/` may reference this module.
//! 3. **Silent degradation.** Any failure to resolve yields `None` — never a
//!    surfaced error, never a delayed snapshot.
//! 4. **v1 sources are user-authored only.** Branch names and commit subjects are
//!    text the user typed. Content-derived sources (prompts, titles) are parked
//!    behind their own gates (spec §10).
//!
//! The v1 source is **git**: the branch comes from a pure read of `<root>/.git`'s
//! `HEAD` (following a worktree/submodule `gitdir:` redirect); the commit subject
//! is best-effort via a spawned `git log -1 --format=%s` with a hard timeout —
//! spawn failure just degrades to branch-only. The seam between the two: all
//! assembly/sanitization is pure and unit-tested; [`git_subject`] is the only
//! spawn, so tests never need a git binary.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Display cap — sanitized context text never exceeds this many chars (spec §4).
const MAX_CHARS: usize = 120;

/// Commit-subject spawn timeout. Cold `\\wsl.localhost` roots can be slow; past
/// this we ship branch-only rather than stall (spec §4 / open item 1).
const SUBJECT_TIMEOUT_SECS: u64 = 2;

/// Where a context string came from. Future content-derived sources (spec §10)
/// each arrive behind their own opt-in gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    Git,
    /// A per-moment string from the Claude Code hooks: your `UserPromptSubmit`
    /// prompt snippet, or the transcript's session summary on `SessionStart`.
    /// Content-derived → opt-in (`context_hook_prompts`, default off) and
    /// **display-only**: it is never stamped onto timeline blocks (the A2
    /// raw-capture posture — prompt-derived text stays off disk).
    HookPrompt,
    /// A per-moment string from the browser extension: the conversation's title
    /// (providers auto-title chats from their content → content-derived). Opt-in
    /// (`context_browser_titles`, default off), double-gated — the extension only
    /// sends titles when the `/raise` poll advertises the setting — and
    /// **display-only**, like every content-derived source.
    BrowserTitle,
}

/// A resolved, display-ready context string for the focused project. Attached to
/// the widget snapshot by the orchestrator; never seen by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextString {
    /// Sanitized, ≤ 120 chars: `"<branch> · <subject>"`, or just the branch.
    pub text: String,
    pub source: ContextSource,
    /// Unix seconds when this string was resolved.
    pub observed_at: i64,
}

/// The persisted shape stamped onto a closed timeline block — text + source only
/// (the block's own times date it; spec §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredContext {
    pub text: String,
    pub source: ContextSource,
}

impl From<&ContextString> for StoredContext {
    fn from(c: &ContextString) -> Self {
        Self { text: c.text.clone(), source: c.source }
    }
}

// --- Roots registry -----------------------------------------------------------

/// Slug → project root directory, populated by the Claude Code adapter as a side
/// effect of the `cwd` read it already does for slug resolution. The orchestrator
/// consults it to know *where* to resolve git context; projects without a root
/// (browser-minted) simply never appear and yield no context (spec §4).
pub type SharedRoots = Arc<Mutex<HashMap<String, PathBuf>>>;

pub fn new_roots() -> SharedRoots {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Record (or refresh) a project's root. Poison-tolerant: attribution must never
/// panic over a display-only side table.
pub fn record_root(roots: &SharedRoots, slug: &str, root: PathBuf) {
    if let Ok(mut g) = roots.lock() {
        g.insert(slug.to_string(), root);
    }
}

pub fn root_for(roots: &SharedRoots, slug: &str) -> Option<PathBuf> {
    roots.lock().ok()?.get(slug).cloned()
}

// --- Moments (per-moment context, spec §10) -----------------------------------

/// Slug → the freshest *per-moment* context string: your last prompt snippet or
/// session summary (hooks receiver), the live conversation title (browser
/// receiver). **Receivers record; the orchestrator arbitrates and clears.**
/// Last-writer-wins across sources — the §10 ladder puts every per-moment string
/// above the per-root git fallback, and the newest one describes *now*. All
/// content-derived, so all in-memory and display-only: nothing here is ever
/// stamped onto a persisted block. The orchestrator clears a slug's moment when
/// its block closes (a moment describes the block it arrived in).
pub type SharedMoments = Arc<Mutex<HashMap<String, ContextString>>>;

pub fn new_moments() -> SharedMoments {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Record (or supersede) a slug's live moment. Poison-tolerant, like the roots
/// registry — a display nicety must never panic a receiver thread.
pub fn record_moment(
    moments: &SharedMoments,
    slug: &str,
    text: &str,
    source: ContextSource,
    observed_at: i64,
) {
    if let Ok(mut g) = moments.lock() {
        g.insert(
            slug.to_string(),
            ContextString { text: text.to_string(), source, observed_at },
        );
    }
}

pub fn moment_for(moments: &SharedMoments, slug: &str) -> Option<ContextString> {
    moments.lock().ok()?.get(slug).cloned()
}

pub fn clear_moment(moments: &SharedMoments, slug: &str) {
    if let Ok(mut g) = moments.lock() {
        g.remove(slug);
    }
}

// --- Per-root cache -----------------------------------------------------------

/// Per-root resolution cache with in-flight dedup (spec §4). `None` results are
/// cached too — a root that isn't a git repo shouldn't be re-probed on every
/// event, only once per TTL.
#[derive(Default)]
pub struct ContextCache {
    entries: HashMap<PathBuf, (Option<ContextString>, i64)>,
    in_flight: HashSet<PathBuf>,
}

impl ContextCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached `(value, fetched_at)` for a root, if any resolution completed.
    pub fn get(&self, root: &Path) -> Option<&(Option<ContextString>, i64)> {
        self.entries.get(root)
    }

    /// The last successfully resolved string for a root, regardless of age — what
    /// gets stamped onto a closing block (spec §5).
    pub fn last(&self, root: &Path) -> Option<ContextString> {
        self.entries.get(root).and_then(|(c, _)| c.clone())
    }

    /// Mark a resolution as started. Returns `false` if one is already in flight
    /// for this root, so the caller spawns at most one resolver per root.
    pub fn begin(&mut self, root: &Path) -> bool {
        self.in_flight.insert(root.to_path_buf())
    }

    /// Store a completed resolution and clear its in-flight mark.
    pub fn store(&mut self, root: &Path, ctx: Option<ContextString>, now: i64) {
        self.in_flight.remove(root);
        self.entries.insert(root.to_path_buf(), (ctx, now));
    }
}

// --- Resolution ---------------------------------------------------------------

/// Resolve the full context string for a project root: branch (required, pure
/// file read) + commit subject (best-effort, spawned). `None` on any branch
/// failure — silent degradation, never an error (spec §2.3).
pub async fn resolve(root: &Path, now: i64) -> Option<ContextString> {
    let branch = branch_from_root(root)?;
    let subject = git_subject(root).await;
    Some(assemble(&branch, subject.as_deref(), now))
}

/// Pure assembly: `"<branch> · <subject>"` (or branch alone), sanitized and
/// capped. The testable half of the seam — no I/O, no spawn.
pub fn assemble(branch: &str, subject: Option<&str>, now: i64) -> ContextString {
    let raw = match subject {
        Some(s) => format!("{branch} · {s}"),
        None => branch.to_string(),
    };
    ContextString {
        text: sanitize(&raw),
        source: ContextSource::Git,
        observed_at: now,
    }
}

/// The current branch, from a pure read of `<root>/.git` (spec §4):
///
/// * `.git` as a *file* (worktree/submodule) → follow its `gitdir:` line,
///   relative paths resolving against `root`.
/// * `HEAD` = `ref: refs/heads/<branch>` → the branch (nested slashes kept).
/// * `HEAD` = bare commit SHA (detached) → `@<first 7>`.
/// * Missing or unparseable anything → `None`.
///
/// No spawn, no libgit2 — the branch *name* lives in the HEAD file itself
/// (packed refs are irrelevant to it).
pub fn branch_from_root(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_file() {
        let redirect = parse_gitfile(&std::fs::read_to_string(&dot_git).ok()?)?;
        if redirect.is_absolute() {
            redirect
        } else {
            root.join(redirect)
        }
    } else if dot_git.is_dir() {
        dot_git
    } else {
        return None;
    };
    parse_head(&std::fs::read_to_string(git_dir.join("HEAD")).ok()?)
}

/// Parse a gitfile's `gitdir: <path>` line. Pure.
fn parse_gitfile(contents: &str) -> Option<PathBuf> {
    contents
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(|p| PathBuf::from(p.trim()))
}

/// Parse a HEAD file into a display branch. Pure — fixture-tested.
fn parse_head(contents: &str) -> Option<String> {
    let line = contents.lines().next()?.trim();
    if let Some(target) = line.strip_prefix("ref:") {
        let branch = target.trim().strip_prefix("refs/heads/")?;
        return (!branch.is_empty()).then(|| branch.to_string());
    }
    // Detached HEAD: a bare full commit id (SHA-1 or SHA-256 hex). Anything else
    // is garbage and yields None rather than a guess.
    if (line.len() == 40 || line.len() == 64) && line.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(format!("@{}", &line[..7]));
    }
    None
}

/// Sanitize for single-line display: drop control characters, collapse whitespace
/// runs to one space, trim, and cap at [`MAX_CHARS`] on a char boundary with `…`.
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_CHARS * 4));
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else if !ch.is_control() {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    if out.chars().count() > MAX_CHARS {
        let mut truncated: String = out.chars().take(MAX_CHARS - 1).collect();
        truncated.truncate(truncated.trim_end().len());
        truncated.push('…');
        out = truncated;
    }
    out
}

/// Last commit subject via `git -C <root> log -1 --format=%s` — the impure half of
/// the seam. Spawn failure, non-zero exit, empty output, or the hard timeout all
/// yield `None` (branch alone still shows). A `git` binary is a soft dependency.
async fn git_subject(root: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["log", "-1", "--format=%s"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true); // a timed-out git must not linger
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash from a GUI app
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(SUBJECT_TIMEOUT_SECS),
        cmd.output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let subject = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!subject.is_empty()).then_some(subject)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- HEAD parser fixtures (spec §9) --------------------------------------

    #[test]
    fn head_parses_normal_branch() {
        assert_eq!(parse_head("ref: refs/heads/main\n").as_deref(), Some("main"));
    }

    #[test]
    fn head_keeps_nested_branch_slashes() {
        assert_eq!(
            parse_head("ref: refs/heads/feat/x/y\n").as_deref(),
            Some("feat/x/y")
        );
    }

    #[test]
    fn head_detached_sha_abbreviates() {
        assert_eq!(
            parse_head("a94a8fe5ccb19ba61c4c0873d391e987982fbbd3\n").as_deref(),
            Some("@a94a8fe")
        );
        // SHA-256 repos have 64-hex HEADs.
        let sha256 = "b".repeat(64);
        assert_eq!(parse_head(&sha256).as_deref(), Some("@bbbbbbb"));
    }

    #[test]
    fn head_garbage_is_none() {
        assert_eq!(parse_head(""), None);
        assert_eq!(parse_head("not a head file"), None);
        assert_eq!(parse_head("ref: refs/heads/"), None);
        // Non-branch ref (rebase leaves these) → None, not a fake branch.
        assert_eq!(parse_head("ref: refs/remotes/origin/main"), None);
        // Short hex is ambiguous garbage, not a detached HEAD.
        assert_eq!(parse_head("deadbeef"), None);
    }

    #[test]
    fn branch_resolves_through_gitfile_redirect() {
        // Worktree layout: <root>/.git is a *file* pointing at the real git dir.
        let base = std::env::temp_dir().join(format!("whence-ctx-wt-{}", std::process::id()));
        let real = base.join("real-gitdir");
        let root = base.join("worktree");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(real.join("HEAD"), "ref: refs/heads/feat/tray-icon\n").unwrap();
        // Relative redirect resolves against root.
        std::fs::write(root.join(".git"), "gitdir: ../real-gitdir\n").unwrap();
        assert_eq!(branch_from_root(&root).as_deref(), Some("feat/tray-icon"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn branch_from_plain_repo_and_missing() {
        let base = std::env::temp_dir().join(format!("whence-ctx-git-{}", std::process::id()));
        let root = base.join("repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(branch_from_root(&root).as_deref(), Some("main"));
        // No .git at all → None (silent degradation).
        assert_eq!(branch_from_root(&base.join("not-a-repo")), None);
        // Garbage HEAD → None.
        std::fs::write(root.join(".git/HEAD"), "???").unwrap();
        assert_eq!(branch_from_root(&root), None);
        std::fs::remove_dir_all(&base).ok();
    }

    // --- Sanitization / truncation (spec §9) ----------------------------------

    #[test]
    fn sanitize_strips_controls_and_collapses_whitespace() {
        assert_eq!(sanitize("  a\tb\n\nc\u{7}d  "), "a b cd");
    }

    #[test]
    fn sanitize_truncates_on_char_boundary() {
        let long = "x".repeat(200);
        let out = sanitize(&long);
        assert_eq!(out.chars().count(), 120);
        assert!(out.ends_with('…'));
        // Multibyte: 200 four-byte chars must cut between chars, never mid-char.
        let emoji = "🦀".repeat(200);
        let out = sanitize(&emoji);
        assert_eq!(out.chars().count(), 120);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().filter(|c| *c == '🦀').count(), 119);
    }

    #[test]
    fn assemble_formats_branch_and_subject() {
        let c = assemble("main", Some("add HEAD parser fixtures"), 42);
        assert_eq!(c.text, "main · add HEAD parser fixtures");
        assert_eq!(c.source, ContextSource::Git);
        assert_eq!(c.observed_at, 42);
        // No subject resolved → branch alone (spec §4).
        assert_eq!(assemble("feat/x", None, 0).text, "feat/x");
    }

    // --- Cache -----------------------------------------------------------------

    #[test]
    fn cache_dedups_in_flight_and_remembers_last() {
        let root = PathBuf::from("/some/repo");
        let mut cache = ContextCache::new();
        assert!(cache.begin(&root));
        assert!(!cache.begin(&root), "second begin must be deduped");
        let ctx = assemble("main", None, 10);
        cache.store(&root, Some(ctx.clone()), 10);
        assert!(cache.begin(&root), "store clears the in-flight mark");
        assert_eq!(cache.get(&root).unwrap().1, 10);
        assert_eq!(cache.last(&root), Some(ctx));
        // A later failed resolve is cached as None — last() reflects it honestly.
        cache.store(&root, None, 20);
        assert_eq!(cache.last(&root), None);
    }

    // --- Tripwire (spec §9, gstack A5 pattern) ---------------------------------

    #[test]
    fn tripwire_engine_and_neuroskill_never_reference_context() {
        // Pins §2 of docs/context-strings.md: context is never an attribution
        // input (the engine neither receives nor emits it) and never enters the
        // NeuroSkill write path. If this fails, the invariant — not this test —
        // is what's being broken.
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for module in ["engine", "neuroskill"] {
            for entry in std::fs::read_dir(src.join(module)).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for needle in ["ContextString", "context::"] {
                    assert!(
                        !text.contains(needle),
                        "{} references `{needle}` — forbidden by docs/context-strings.md §2 \
                         (context is display-only: never an attribution input, never in labels)",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn tripwire_labels_stay_byte_identical() {
        // §2 invariant 1: the label for a context-bearing project is exactly what
        // it was before context strings existed — nothing appended, ever.
        assert_eq!(
            crate::neuroskill::start_label("whence"),
            "Whence:project=whence:start"
        );
        assert_eq!(
            crate::neuroskill::end_label("whence"),
            "Whence:project=whence:end"
        );
    }
}
