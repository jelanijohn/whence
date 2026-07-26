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
//! [`watch`] returns the watcher handle to store in app state.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

use super::{slug_from_transcript_dir, slugify, Surface, WorkEvent, WorkKind};
use crate::context::{self, SharedRoots};

/// The user's home directory. `HOME` on Unix; Windows doesn't set it, so fall back
/// to `USERPROFILE` — the assumption that Whence always runs under WSL2 (where
/// `HOME` exists) died when we started shipping native builds.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Resolve the Claude Code config dir (the `.claude` folder holding `projects/`
/// and `settings.json`) — shared by the transcript watcher and the hooks
/// installer so both always target the *same* Claude Code installation:
///
/// 1. **Explicit override** (`claude_dir` setting) — the user's last word.
/// 2. **Native home** (`$HOME`/`%USERPROFILE%`), if it actually has a
///    `projects/` dir — i.e. Claude Code has run on this OS.
/// 3. **WSL discovery** (Windows only) — Claude Code often runs *inside* WSL
///    while Whence runs on the host; walk `\\wsl$\<distro>` home dirs for a
///    `.claude/projects` tree.
/// 4. The native home again, existing or not — first-run fallback (the watcher
///    creates `projects/` so the recursive watch has something to attach to).
pub fn claude_dir(override_path: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = override_path {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let native = home_dir().map(|h| h.join(".claude"));
    if let Some(dir) = &native {
        if dir.join("projects").is_dir() {
            return Some(dir.clone());
        }
    }
    #[cfg(target_os = "windows")]
    if let Some(dir) = wsl_claude_dir() {
        return Some(dir);
    }
    native
}

/// `<claude_dir>/projects` — the root of Claude Code's per-project transcripts.
pub fn transcripts_root(claude_dir_override: Option<&str>) -> Option<PathBuf> {
    Some(claude_dir(claude_dir_override)?.join("projects"))
}

/// Find a `.claude` dir inside a WSL distro from the Windows host, via the
/// `\\wsl$\` filesystem bridge. Checks `/root` first (WSL setups often run as
/// root), then each `/home/<user>`, per distro in `wsl -l` order (default distro
/// first). Only returns a dir that already has `projects/` — existence is the
/// signal that Claude Code actually runs there.
#[cfg(target_os = "windows")]
fn wsl_claude_dir() -> Option<PathBuf> {
    for distro in wsl_distros() {
        let fs_root = PathBuf::from(format!(r"\\wsl$\{distro}"));
        let mut homes = vec![fs_root.join("root")];
        if let Ok(entries) = std::fs::read_dir(fs_root.join("home")) {
            let mut users: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            users.sort();
            homes.extend(users);
        }
        for home in homes {
            let dir = home.join(".claude");
            if dir.join("projects").is_dir() {
                return Some(dir);
            }
        }
    }
    None
}

/// Installed WSL distro names via `wsl.exe -l -q` (default distro listed first).
/// Empty when WSL isn't installed. `wsl.exe` emits UTF-16LE unless `WSL_UTF8=1`,
/// so sniff for NUL bytes and decode accordingly.
#[cfg(target_os = "windows")]
fn wsl_distros() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    // Don't flash a console window from a GUI app.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(out) = std::process::Command::new("wsl.exe")
        .args(["-l", "-q"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    let text = if out.stdout.iter().take(64).any(|b| *b == 0) {
        let wide: Vec<u16> = out
            .stdout
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&wide)
    } else {
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    text.lines()
        .map(|l| l.trim_matches(['\0', ' ', '\r']).to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Is this a network (UNC) path — e.g. `\\wsl$\<distro>\...`? OS file-change
/// notifications don't traverse the 9P bridge, so a watcher on such a root must
/// poll. Always false on Unix (no path prefixes).
fn is_network_path(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    match path.components().next() {
        Some(Component::Prefix(pre)) => {
            matches!(pre.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
        }
        _ => false,
    }
}

/// Start watching the transcript tree, emitting `WorkEvent`s on `tx`. Returns the
/// watcher handle (keep it alive) or an error if the root can't be watched. A
/// missing root isn't an error — Claude Code may not have run yet; we create the
/// directory so the recursive watch has something to attach to.
///
/// `aliases` maps transcript **directory name** → canonical slug (user override).
/// The watcher also keeps a per-directory **cache** of cwd-derived slugs so it
/// reads each transcript's `cwd` only once, not on every append.
///
/// Boxed because the backend varies: a native root gets the OS-notification
/// watcher; a network root (a `\\wsl$\` tree, where those notifications never
/// arrive) gets a mtime-polling watcher instead.
pub fn watch(
    tx: UnboundedSender<WorkEvent>,
    aliases: HashMap<String, String>,
    claude_dir_override: Option<&str>,
    roots: SharedRoots,
) -> Result<Box<dyn Watcher + Send>, String> {
    let root = transcripts_root(claude_dir_override)
        .ok_or("could not resolve a home directory (HOME/USERPROFILE) for ~/.claude/projects")?;
    if !root.exists() {
        std::fs::create_dir_all(&root)
            .map_err(|e| format!("could not create {}: {e}", root.display()))?;
    }

    // dir-name → slug, seeded empty; filled with stable (alias/cwd) resolutions so
    // we don't re-read a transcript's cwd on every subsequent append.
    let mut cache: HashMap<String, String> = HashMap::new();
    let handler = move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        let is_create = matches!(event.kind, EventKind::Create(_));
        let is_write = matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_));
        if !is_write {
            return;
        }
        for path in &event.paths {
            if let Some(ev) = event_for_path(path, is_create, &aliases, &mut cache, &roots) {
                // Unbounded send never blocks; an error only means the core task
                // is gone (shutting down), so dropping the event is correct.
                let _ = tx.send(ev);
            }
        }
    };

    let mut watcher: Box<dyn Watcher + Send> = if is_network_path(&root) {
        let config = notify::Config::default()
            .with_poll_interval(std::time::Duration::from_secs(2));
        Box::new(
            notify::PollWatcher::new(handler, config)
                .map_err(|e| format!("could not create transcript poll watcher: {e}"))?,
        )
    } else {
        Box::new(
            notify::recommended_watcher(handler)
                .map_err(|e| format!("could not create transcript watcher: {e}"))?,
        )
    };

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
    roots: &SharedRoots,
) -> Option<WorkEvent> {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return None;
    }
    let dir_name = path.parent()?.file_name()?.to_str()?;
    let project = resolve_slug(dir_name, path, aliases, cache, roots);
    Some(WorkEvent {
        ts: chrono::Utc::now().to_rfc3339(),
        surface: Surface::ClaudeCode,
        project,
        // The transcript file stem *is* the Claude Code session UUID (the file is
        // `<session_id>.jsonl`), so it's a stable per-session source key — and it matches
        // what the hooks receiver derives from `transcript_path`, so the two CC surfaces
        // agree on which source a session is.
        source: path.file_stem().and_then(|s| s.to_str()).map(str::to_string),
        source_label: None, // engine labels it "claude code"
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
/// 1. **Cached result** — a stable (alias/cwd) resolution we already made for this
///    dir; the root was recorded alongside it.
/// 2. **Alias override** (`dir_name` → slug) — the user's explicit last word.
/// 3. **`cwd` from the transcript itself** — the launch directory's basename, the
///    only lossless source (the dir name's `/`→`-` encoding is ambiguous). Cached.
/// 4. **Dir-name heuristic** — the lossy trailing-segment fallback, *not* cached so
///    a later read can upgrade it once the session writes its first `cwd` line.
///
/// The `cwd` read also feeds the context-string **roots registry** (slug → project
/// root, docs/context-strings.md §4) — the same single read, one extra side table.
/// A dir is only cached once its root is recorded, so an aliased dir whose
/// transcript hasn't written `cwd` yet keeps retrying, exactly like step 4.
fn resolve_slug(
    dir_name: &str,
    path: &Path,
    aliases: &HashMap<String, String>,
    cache: &mut HashMap<String, String>,
    roots: &SharedRoots,
) -> Option<String> {
    if let Some(slug) = cache.get(dir_name) {
        return Some(slug.clone());
    }
    let cwd = first_cwd(path);
    // The slug: alias wins; else the cwd basename (the lossless source).
    let slug = match aliases.get(dir_name) {
        Some(alias) => Some(alias.clone()),
        None => cwd.as_deref().and_then(|c| slugify(cwd_basename(c)?)),
    };
    match (slug, cwd) {
        (Some(slug), Some(cwd)) => {
            context::record_root(roots, &slug, rebase_root(&cwd, path));
            cache.insert(dir_name.to_string(), slug.clone());
            Some(slug)
        }
        // Alias with no cwd yet: the slug is stable but there's no root to record —
        // don't cache, so a later append can pick the root up.
        (Some(slug), None) => Some(slug),
        (None, _) => slug_from_transcript_dir(dir_name),
    }
}

/// A cwd's final path segment, tolerating both separators (a Windows-style cwd
/// must basename correctly too).
fn cwd_basename(cwd: &str) -> Option<&str> {
    cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next()
}

/// The slug derived from a transcript's launch cwd — [`first_cwd`]'s basename
/// through the shared `slugify`. The hooks receiver resolves through this too, so
/// both Claude Code surfaces agree on a session's project.
pub(crate) fn slug_from_cwd(path: &Path) -> Option<String> {
    slugify(cwd_basename(&first_cwd(path)?)?)
}

/// Read a transcript's launch directory from its **first** `cwd` line — the
/// project root, even if the session later `cd`s into a subdir. Its basename is
/// the slug (run through the shared `slugify` so an fs-resolved slug converges
/// with a browser-minted one for the same name, §5/§9); the path itself is the
/// context-string root. `None` if the file is unreadable, empty, or predates the
/// `cwd` field (older transcripts). Stops at the first match, so it's cheap on
/// large files.
///
/// Caveat: a transcript synced from another machine carries that machine's `cwd`
/// (e.g. a macOS path under a Linux dir). That only affects *historical* files;
/// the session you're actively working in is local, so its `cwd` is correct.
pub(crate) fn first_cwd(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(cwd) = val.get("cwd").and_then(|c| c.as_str()) {
            return Some(cwd.to_string());
        }
    }
    None
}

/// Turn a transcript-reported cwd into a path *this* process can reach. The one
/// interesting case: Whence running natively on Windows watching a `\\wsl$\<distro>`
/// transcript tree — the cwd inside is a Unix path on that distro, so rebase it
/// onto the tree's UNC prefix. Everywhere else the cwd is local and used as-is.
fn rebase_root(cwd: &str, transcript_path: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    if cwd.starts_with('/') {
        if let Some(prefix) = unc_prefix(transcript_path) {
            let mut root = prefix;
            for seg in cwd.split('/').filter(|s| !s.is_empty()) {
                root.push(seg);
            }
            return root;
        }
    }
    let _ = transcript_path;
    PathBuf::from(cwd)
}

/// The UNC server+share prefix (`\\wsl$\<distro>`) of a network path, if any.
#[cfg(target_os = "windows")]
fn unc_prefix(path: &Path) -> Option<PathBuf> {
    use std::path::{Component, Prefix};
    match path.components().next()? {
        Component::Prefix(pre) => match pre.kind() {
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) => Some(PathBuf::from(pre.as_os_str())),
            _ => None,
        },
        _ => None,
    }
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
        let ev = event_for_path(&p, false, &no_aliases(), &mut cache, &context::new_roots()).unwrap();
        assert_eq!(ev.project.as_deref(), Some("waid"));
        assert_eq!(ev.surface, Surface::ClaudeCode);
        assert_eq!(ev.kind, WorkKind::Active);
        assert_eq!(ev.confidence, 1.0);
        // The transcript file stem is the per-session source key (the session UUID).
        assert_eq!(ev.source.as_deref(), Some("abc123"));
    }

    #[test]
    fn new_file_is_session_start() {
        let p = PathBuf::from("/home/u/.claude/projects/-root-Projects-whence/s.jsonl");
        let mut cache = HashMap::new();
        let ev = event_for_path(&p, true, &no_aliases(), &mut cache, &context::new_roots()).unwrap();
        assert_eq!(ev.kind, WorkKind::SessionStart);
        assert_eq!(ev.project.as_deref(), Some("whence"));
    }

    #[test]
    fn ignores_non_jsonl() {
        let p = PathBuf::from("/home/u/.claude/projects/-root-Projects-waid/notes.txt");
        let mut cache = HashMap::new();
        assert!(event_for_path(&p, false, &no_aliases(), &mut cache, &context::new_roots()).is_none());
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
        let roots = context::new_roots();
        let ev = event_for_path(&p, false, &no_aliases(), &mut cache, &roots).unwrap();
        assert_eq!(ev.project.as_deref(), Some("blapp-web"));
        // First (launch) cwd wins over the later subdir cwd, and it's cached.
        assert_eq!(cache.get("-root-Projects-blapp-web").map(String::as_str), Some("blapp-web"));
        // The same read feeds the context-string roots registry: slug → launch dir.
        assert_eq!(
            context::root_for(&roots, "blapp-web"),
            Some(PathBuf::from("/root/Projects/blapp-web"))
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn claude_dir_override_wins() {
        let d = claude_dir(Some("/custom/.claude")).unwrap();
        assert_eq!(d, PathBuf::from("/custom/.claude"));
        // Blank/whitespace override = "use default", not an empty path.
        assert_ne!(claude_dir(Some("  ")), Some(PathBuf::from("")));
    }

    #[test]
    fn network_path_detection() {
        // Unix-style and relative paths are never network roots.
        assert!(!is_network_path(Path::new("/root/.claude/projects")));
        assert!(!is_network_path(Path::new("projects")));
        // UNC prefixes only parse as such on Windows, where the poll fallback lives.
        #[cfg(target_os = "windows")]
        {
            assert!(is_network_path(Path::new(r"\\wsl$\Ubuntu\root\.claude\projects")));
            assert!(is_network_path(Path::new(r"\\wsl.localhost\Ubuntu\root\.claude")));
            assert!(!is_network_path(Path::new(r"C:\Users\u\.claude\projects")));
        }
    }

    #[test]
    fn alias_overrides_everything() {
        let p = PathBuf::from("/home/u/.claude/projects/-root-Projects-glue-mac/s.jsonl");
        let mut aliases = HashMap::new();
        aliases.insert("-root-Projects-glue-mac".to_string(), "glue-mac".to_string());
        let mut cache = HashMap::new();
        let ev = event_for_path(&p, false, &aliases, &mut cache, &context::new_roots()).unwrap();
        assert_eq!(ev.project.as_deref(), Some("glue-mac"));
    }

    #[test]
    fn alias_records_root_under_alias_slug() {
        // The alias wins the slug, but the cwd read still records the project root
        // under *that* slug — an aliased project gets context strings too.
        let dir = std::env::temp_dir().join("-root-Projects-glue-mac-alias");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("session.jsonl");
        std::fs::write(&p, "{\"type\":\"user\",\"cwd\":\"/root/Projects/glue-mac\"}\n").unwrap();
        let mut aliases = HashMap::new();
        aliases.insert(
            "-root-Projects-glue-mac-alias".to_string(),
            "glue-mac".to_string(),
        );
        let mut cache = HashMap::new();
        let roots = context::new_roots();
        let ev = event_for_path(&p, false, &aliases, &mut cache, &roots).unwrap();
        assert_eq!(ev.project.as_deref(), Some("glue-mac"));
        assert_eq!(
            context::root_for(&roots, "glue-mac"),
            Some(PathBuf::from("/root/Projects/glue-mac"))
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
