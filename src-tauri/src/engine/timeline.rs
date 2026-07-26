//! The local focus-timeline store — **JSONL, one [`FocusBlock`] per line**.
//!
//! Why JSONL over SQLite (principle #7 asked for "SQLite or JSONL"):
//!
//! * **The workload is tiny and append-only.** A handful of blocks close per day;
//!   there are no joins, no ad-hoc queries, no concurrent writers (single-instance
//!   guarantees one process). "Today's blocks" is a date-filtered scan of a file
//!   that stays in the low thousands of lines per year — trivial.
//! * **Inspectability is the *point* of principle #7.** A block is a human-readable
//!   line you can `cat`, `grep`, `tail -f`, and diff. Export is `cp`. SQLite would
//!   need a tool to read and a schema to migrate, buying nothing this store needs.
//! * **It matches the surfaces.** Claude Code's own transcripts are JSONL; the
//!   segmentation fixtures are JSONL event sequences. One format end to end means
//!   we can replay a raw event log through `segment.rs` to re-tune the debounce
//!   offline — the kind of thing a binary store makes annoying.
//!
//! SQLite earns its place elsewhere (the optional read-only NeuroSkill EEG
//! read-back reads *their* SQLite), but for Whence's own timeline JSONL is the
//! better fit, not a compromise.
//!
//! Durability: each block is appended as a single line with a trailing `\n` via
//! one `write_all`. A crash mid-write can leave a truncated trailing line; the
//! reader tolerates it by skipping any line that doesn't parse.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{Local, TimeZone};
use serde::{de::DeserializeOwned, Serialize};

use crate::engine::segment::FocusBlock;

/// A record with a block start time — what the day filter keys on. Generic so the
/// store can hold the bare engine [`FocusBlock`] *or* the orchestrator's stamped
/// record without this module knowing about the extras (the engine tree stays
/// free of display-only concerns).
pub trait HasStart {
    fn start_secs(&self) -> i64;
}

impl HasStart for FocusBlock {
    fn start_secs(&self) -> i64 {
        self.start
    }
}

/// Resolve the timeline file path under the app data dir (`<data>/timeline.jsonl`).
pub fn timeline_path(data_dir: &Path) -> PathBuf {
    data_dir.join("timeline.jsonl")
}

/// Append one closed block as a JSON line. Best-effort durability (see module
/// docs); returns the I/O error so the caller can log it without crashing.
pub fn append_block<T: Serialize>(path: &Path, block: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(block).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// Read every block, skipping any unparseable (e.g. crash-truncated) line. Missing
/// file → empty vec.
pub fn read_all<T: DeserializeOwned>(path: &Path) -> std::io::Result<Vec<T>> {
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(b) = serde_json::from_str::<T>(&line) {
            out.push(b);
        }
        // Unparseable line → skip (tolerate a truncated trailing record).
    }
    Ok(out)
}

/// Blocks whose `start` falls on the local calendar day containing `now` (unix
/// seconds), oldest first — drives the widget's expanded timeline.
pub fn read_day<T: DeserializeOwned + HasStart>(path: &Path, now: i64) -> std::io::Result<Vec<T>> {
    let (day_start, day_end) = local_day_bounds(now);
    let mut blocks = read_all::<T>(path)?;
    blocks.retain(|b| b.start_secs() >= day_start && b.start_secs() < day_end);
    blocks.sort_by_key(|b| b.start_secs());
    Ok(blocks)
}

/// Local midnight-to-midnight bounds (unix seconds) for the day containing `now`.
fn local_day_bounds(now: i64) -> (i64, i64) {
    let dt = Local
        .timestamp_opt(now, 0)
        .single()
        .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap());
    let start_of_day = dt
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|naive| Local.from_local_datetime(&naive).single())
        .map(|d| d.timestamp())
        .unwrap_or(now);
    (start_of_day, start_of_day + 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(project: &str, start: i64, end: i64) -> FocusBlock {
        FocusBlock {
            project: project.into(),
            start,
            end,
            event_count: 3,
            mean_confidence: 0.9,
        }
    }

    #[test]
    fn append_then_read_roundtrips() {
        let dir = std::env::temp_dir().join(format!("whence-tl-{}", std::process::id()));
        let path = timeline_path(&dir);
        let _ = std::fs::remove_file(&path);

        append_block(&path, &block("waid", 100, 200)).unwrap();
        append_block(&path, &block("whoami", 300, 450)).unwrap();
        let all = read_all::<FocusBlock>(&path).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].project, "waid");
        assert_eq!(all[1].end, 450);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_all_skips_garbage_lines() {
        let dir = std::env::temp_dir().join(format!("whence-tl-g-{}", std::process::id()));
        let path = timeline_path(&dir);
        let _ = std::fs::remove_file(&path);
        append_block(&path, &block("waid", 100, 200)).unwrap();
        // Simulate a crash-truncated trailing record.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"project\":\"whoami\",\"start\":3").unwrap();
        drop(f);
        let all = read_all::<FocusBlock>(&path).unwrap();
        assert_eq!(all.len(), 1); // the good line survives, the garbage is skipped
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_empty() {
        let path = std::env::temp_dir().join("whence-nope-xyz.jsonl");
        let _ = std::fs::remove_file(&path);
        assert!(read_all::<FocusBlock>(&path).unwrap().is_empty());
    }
}
