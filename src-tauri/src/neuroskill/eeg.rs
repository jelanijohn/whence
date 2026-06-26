//! Optional **read-only** EEG read-back — powers the widget's intensity meter
//! (§8 "optional read-back", open decision §14.6). The write path is the v1
//! requirement; this is polish, gated behind the `eeg-readback` feature so a v1
//! build doesn't pull SQLite unless asked.
//!
//! Read-scope discipline (principle #5, mirroring WAID): the DB is opened
//! `mode=ro&immutable=1` and the only SELECT issued is [`EEG_QUERY`] against
//! `eeg_timeseries`. Nothing else is read; nothing is written.

use std::path::{Path, PathBuf};

/// The sole query issued against NeuroSkill's EEG table. Read-only; references no
/// other table (see the read-scope guard test). The schema matches WAID's proven
/// reader: each row is `(ts, metrics)` where `metrics` is a JSON blob — `focus` is
/// a *field* inside it, not a column.
pub const EEG_QUERY: &str =
    "SELECT ts, metrics FROM eeg_timeseries WHERE ts >= ?1 AND ts <= ?2 ORDER BY ts";

/// NeuroSkill's EEG store filename, inside the data dir (alongside `labels.sqlite`).
const ACTIVITY_DB: &str = "activity.sqlite";

/// Pull the scalar `focus` metric (0..100) out of an epoch's `metrics` JSON blob.
/// `None` if absent or non-numeric. Mirrors WAID's `parse_metrics`, narrowed to the
/// one field the intensity meter needs.
fn parse_focus(metrics_json: &str) -> Option<f64> {
    serde_json::from_str::<serde_json::Value>(metrics_json)
        .ok()?
        .get("focus")?
        .as_f64()
}

/// Open a NeuroSkill SQLite file read-only and immutable — a static snapshot that
/// never contends with the always-on daemon's write lock.
fn open_ro(db_path: &Path) -> Result<rusqlite::Connection, String> {
    use rusqlite::OpenFlags;
    if !db_path.exists() {
        return Err(format!("NeuroSkill data file not found: {}", db_path.display()));
    }
    let uri = format!("file:{}?mode=ro&immutable=1", db_path.to_string_lossy());
    rusqlite::Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("could not open {} read-only: {e}", db_path.display()))
}

/// Mean `focus` (0..100) across EEG epochs in `[start, end]`. `None` if there are
/// no epochs (or none carrying a `focus` metric) in the window. Read-only.
pub fn mean_focus(activity_db: &Path, start: i64, end: i64) -> Result<Option<f64>, String> {
    let conn = open_ro(activity_db)?;
    let mut stmt = conn
        .prepare(EEG_QUERY)
        .map_err(|e| format!("NeuroSkill eeg query failed: {e}"))?;
    let rows = stmt
        .query_map([start, end], |row| row.get::<_, String>(1))
        .map_err(|e| format!("NeuroSkill eeg read failed: {e}"))?;
    let mut sum = 0.0;
    let mut n = 0u32;
    for r in rows {
        let metrics = r.map_err(|e| format!("NeuroSkill eeg row failed: {e}"))?;
        if let Some(focus) = parse_focus(&metrics) {
            sum += focus;
            n += 1;
        }
    }
    Ok((n > 0).then(|| sum / n as f64))
}

/// The native NeuroSkill data directory (`<localdata>/NeuroSkill`): `%LOCALAPPDATA%`
/// on Windows, `~/Library/Application Support` on macOS, `$XDG_DATA_HOME`/`~/.local/share`
/// on Linux. `None` if no home/data dir resolves. Note this is the daemon's *Local*
/// AppData — distinct from the auth token's *Roaming* path (`client::default_token_path`).
pub fn default_data_dir() -> Option<PathBuf> {
    let base = if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        PathBuf::from(x)
    } else if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
    } else if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support")
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".local/share")
    };
    Some(base.join("NeuroSkill"))
}

/// Discover the Windows-host EEG store from inside WSL2 by walking
/// `/mnt/<drive>/Users/<user>/AppData/Local/NeuroSkill/activity.sqlite`. Under WSL2
/// the daemon runs on the Windows host, so its store lives under the host's *Local*
/// AppData — the same split `client::wsl_token_path` handles for the token, only the
/// token is *Roaming*. First existing match (drives and users sorted for determinism).
fn wsl_activity_db() -> Option<PathBuf> {
    let mnt = Path::new("/mnt");
    let mut drives: Vec<PathBuf> = std::fs::read_dir(mnt)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    drives.sort();
    for drive in drives {
        let users = drive.join("Users");
        let Ok(entries) = std::fs::read_dir(&users) else {
            continue;
        };
        let mut user_dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        user_dirs.sort();
        for user in user_dirs {
            let candidate = user.join("AppData/Local/NeuroSkill").join(ACTIVITY_DB);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Resolve the EEG `activity.sqlite` path, honoring an explicit data-dir override
/// first, then the native data dir if the file exists, then WSL2 Windows-host
/// discovery. `None` when no candidate file can be found — the intensity meter then
/// stays hidden. Mirrors the order in `client::resolve_token_path`.
pub fn resolve_activity_db(data_dir_override: Option<&str>) -> Option<PathBuf> {
    if let Some(d) = data_dir_override {
        let d = d.trim();
        if !d.is_empty() {
            return Some(PathBuf::from(d).join(ACTIVITY_DB));
        }
    }
    if let Some(dir) = default_data_dir() {
        let p = dir.join(ACTIVITY_DB);
        if p.is_file() {
            return Some(p);
        }
    }
    if crate::neuroskill::client::is_wsl() {
        if let Some(p) = wsl_activity_db() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod read_scope_guard {
    use super::EEG_QUERY;

    #[test]
    fn query_is_read_only_and_scoped() {
        let lower = EEG_QUERY.to_lowercase();
        assert!(lower.trim_start().starts_with("select"));
        for kw in ["insert", "update ", "delete", "drop", "attach", "pragma", "create"] {
            assert!(!lower.contains(kw), "non-read keyword `{kw}` in EEG query");
        }
        assert!(lower.contains("eeg_timeseries"));
    }
}

#[cfg(test)]
mod parse {
    use super::parse_focus;

    #[test]
    fn pulls_focus_scalar_from_metrics_blob() {
        // `focus` lives inside the metrics JSON, alongside fields we ignore.
        assert_eq!(
            parse_focus(r#"{"focus": 73.5, "engagement": 40, "channels": [1,2]}"#),
            Some(73.5),
        );
        // Integer is fine (serde coerces to f64).
        assert_eq!(parse_focus(r#"{"focus": 50}"#), Some(50.0));
    }

    #[test]
    fn none_when_focus_absent_or_unparseable() {
        assert_eq!(parse_focus(r#"{"engagement": 40}"#), None);
        assert_eq!(parse_focus(r#"{"focus": "high"}"#), None); // non-numeric
        assert_eq!(parse_focus("not json"), None);
        assert_eq!(parse_focus("[1,2,3]"), None); // array, not an object
    }
}
