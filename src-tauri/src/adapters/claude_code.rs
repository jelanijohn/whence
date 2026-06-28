//! Claude Code adapter (primary, v1) — the richest, cleanest signal.
//!
//! **Transcript watch (zero-config baseline).** Claude Code writes per-project
//! session transcripts under `~/.claude/projects/<encoded-cwd>/*.jsonl`. Watching
//! that tree with `notify` gives, with no setup:
//!   * **project** ← the encoded directory name (`slug_from_transcript_dir`)
//!   * **activity/timing** ← lines appended to the active transcript
//!
//! A new `*.jsonl` file → `SessionStart`; an append to an existing one → `Active`.
//! Distinguishing `prompt` vs `tool_use` (and the live `awaiting_input` status)
//! is the job of the **hooks** receiver (v1.5) — richer than anything transcript
//! mtimes can infer. This module ships first because it works immediately.
//!
//! The watcher must be kept alive by the caller (dropping it stops watching), so
//! [`watch`] returns the `RecommendedWatcher` handle to store in app state.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

use super::{slug_from_transcript_dir, slugify, Surface, WorkEvent, WorkKind};

/// `~/.claude/projects` — the root of Claude Code's per-project transcripts.
pub fn transcripts_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".claude").join("projects"))
}

/// Start watching the transcript tree, emitting `WorkEvent`s on `tx`. Returns the
/// watcher handle (keep it alive) or an error if the root can't be watched. A
/// missing root isn't an error — Claude Code may not have run yet; we create the
/// directory so the recursive watch has something to attach to.
///
/// `aliases` maps transcript **directory name** → canonical slug (user override).
/// The watcher also keeps a per-directory **cache** of cwd-derived slugs so it
/// reads each transcript's `cwd` only once, not on every append.
pub fn watch(
    tx: UnboundedSender<WorkEvent>,
    aliases: HashMap<String, String>,
) -> Result<RecommendedWatcher, String> {
    let root = transcripts_root().ok_or("could not resolve $HOME for ~/.claude/projects")?;
    if !root.exists() {
        std::fs::create_dir_all(&root)
            .map_err(|e| format!("could not create {}: {e}", root.display()))?;
    }

    // dir-name → slug, seeded empty; filled with stable (alias/cwd) resolutions so
    // we don't re-read a transcript's cwd on every subsequent append.
    let mut cache: HashMap<String, String> = HashMap::new();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        let is_create = matches!(event.kind, EventKind::Create(_));
        let is_write = matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_));
        if !is_write {
            return;
        }
        for path in &event.paths {
            if let Some(ev) = event_for_path(path, is_create, &aliases, &mut cache) {
                // Unbounded send never blocks; an error only means the core task
                // is gone (shutting down), so dropping the event is correct.
                let _ = tx.send(ev);
            }
        }
    })
    .map_err(|e| format!("could not create transcript watcher: {e}"))?;

    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| format!("could not watch {}: {e}", root.display()))?;
    Ok(watcher)
}

/// Build a `WorkEvent` from a changed transcript path, or `None` if the path isn't
/// a `*.jsonl` transcript we can attribute. The clock read for `ts` is the only
/// impurity beyond the cwd file read in [`resolve_slug`]. `cache` memoizes stable
/// slug resolutions per directory.
fn event_for_path(
    path: &Path,
    is_create: bool,
    aliases: &HashMap<String, String>,
    cache: &mut HashMap<String, String>,
) -> Option<WorkEvent> {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return None;
    }
    let dir_name = path.parent()?.file_name()?.to_str()?;
    let project = resolve_slug(dir_name, path, aliases, cache);
    Some(WorkEvent {
        ts: chrono::Utc::now().to_rfc3339(),
        surface: Surface::ClaudeCode,
        project,
        kind: if is_create {
            WorkKind::SessionStart
        } else {
            WorkKind::Active
        },
        confidence: 1.0, // cwd-derived attribution is high-confidence
        detail: None,
    })
}

/// Resolve the project slug for a transcript, preferring the most reliable signal:
///
/// 1. **Alias override** (`dir_name` → slug) — the user's explicit last word.
/// 2. **Cached cwd result** — a slug we already derived from this dir's `cwd`.
/// 3. **`cwd` from the transcript itself** — the launch directory's basename, the
///    only lossless source (the dir name's `/`→`-` encoding is ambiguous). Cached.
/// 4. **Dir-name heuristic** — the lossy trailing-segment fallback, *not* cached so
///    a later read can upgrade it once the session writes its first `cwd` line.
///
/// Steps 1–3 produce a stable slug for the session; only step 4 is provisional.
fn resolve_slug(
    dir_name: &str,
    path: &Path,
    aliases: &HashMap<String, String>,
    cache: &mut HashMap<String, String>,
) -> Option<String> {
    if let Some(slug) = aliases.get(dir_name) {
        return Some(slug.clone());
    }
    if let Some(slug) = cache.get(dir_name) {
        return Some(slug.clone());
    }
    if let Some(slug) = slug_from_cwd(path) {
        cache.insert(dir_name.to_string(), slug.clone());
        return Some(slug);
    }
    slug_from_transcript_dir(dir_name)
}

/// Read a transcript's launch directory from its **first** `cwd` line and return
/// that path's basename — the project root, even if the session later `cd`s into a
/// subdir. `None` if the file is unreadable, empty, or predates the `cwd` field
/// (older transcripts). Stops at the first match, so it's cheap on large files.
///
/// Caveat: a transcript synced from another machine carries that machine's `cwd`
/// (e.g. a macOS path under a Linux dir). That only affects *historical* files;
/// the session you're actively working in is local, so its `cwd` is correct.
pub(crate) fn slug_from_cwd(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(cwd) = val.get("cwd").and_then(|c| c.as_str()) {
            // Split on both separators so a Windows-style cwd basenames correctly,
            // then run the basename through the shared `slugify` so an fs-resolved slug
            // converges with a browser-minted one for the same project name (§5/§9).
            let base = cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next()?;
            if let Some(slug) = slugify(base) {
                return Some(slug);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write;

    fn no_aliases() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn attributes_jsonl_under_project_dir() {
        // Nonexistent file → no cwd to read → dir-name fallback.
        let p = PathBuf::from("/home/u/.claude/projects/-root-Projects-waid/abc123.jsonl");
        let mut cache = HashMap::new();
        let ev = event_for_path(&p, false, &no_aliases(), &mut cache).unwrap();
        assert_eq!(ev.project.as_deref(), Some("waid"));
        assert_eq!(ev.surface, Surface::ClaudeCode);
        assert_eq!(ev.kind, WorkKind::Active);
        assert_eq!(ev.confidence, 1.0);
    }

    #[test]
    fn new_file_is_session_start() {
        let p = PathBuf::from("/home/u/.claude/projects/-root-Projects-whence/s.jsonl");
        let mut cache = HashMap::new();
        let ev = event_for_path(&p, true, &no_aliases(), &mut cache).unwrap();
        assert_eq!(ev.kind, WorkKind::SessionStart);
        assert_eq!(ev.project.as_deref(), Some("whence"));
    }

    #[test]
    fn ignores_non_jsonl() {
        let p = PathBuf::from("/home/u/.claude/projects/-root-Projects-waid/notes.txt");
        let mut cache = HashMap::new();
        assert!(event_for_path(&p, false, &no_aliases(), &mut cache).is_none());
    }

    #[test]
    fn cwd_basename_beats_lossy_dir_name() {
        // The hyphenated-name defect: dir-name resolves to "web", but the real cwd
        // gives "blapp-web". Write a transcript with a cwd line and assert we read it.
        let dir = std::env::temp_dir().join("-root-Projects-blapp-web");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("session.jsonl");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "{{\"type\":\"meta\",\"sessionId\":\"x\"}}").unwrap();
        writeln!(f, "{{\"type\":\"user\",\"cwd\":\"/root/Projects/blapp-web\"}}").unwrap();
        writeln!(f, "{{\"type\":\"user\",\"cwd\":\"/root/Projects/blapp-web/src\"}}").unwrap();

        let mut cache = HashMap::new();
        let ev = event_for_path(&p, false, &no_aliases(), &mut cache).unwrap();
        assert_eq!(ev.project.as_deref(), Some("blapp-web"));
        // First (launch) cwd wins over the later subdir cwd, and it's cached.
        assert_eq!(cache.get("-root-Projects-blapp-web").map(String::as_str), Some("blapp-web"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn alias_overrides_everything() {
        let p = PathBuf::from("/home/u/.claude/projects/-root-Projects-glue-mac/s.jsonl");
        let mut aliases = HashMap::new();
        aliases.insert("-root-Projects-glue-mac".to_string(), "glue-mac".to_string());
        let mut cache = HashMap::new();
        let ev = event_for_path(&p, false, &aliases, &mut cache).unwrap();
        assert_eq!(ev.project.as_deref(), Some("glue-mac"));
    }
}
