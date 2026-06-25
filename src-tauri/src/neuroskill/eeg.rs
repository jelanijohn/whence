//! Optional **read-only** EEG read-back — powers the widget's intensity meter
//! (§8 "optional read-back", open decision §14.6). The write path is the v1
//! requirement; this is polish, gated behind the `eeg-readback` feature so a v1
//! build doesn't pull SQLite unless asked.
//!
//! Read-scope discipline (principle #5, mirroring WAID): the DB is opened
//! `mode=ro&immutable=1` and the only SELECT issued is [`EEG_QUERY`] against
//! `eeg_timeseries`. Nothing else is read; nothing is written.

use std::path::Path;

/// The sole query issued against NeuroSkill's EEG table. Read-only; references no
/// other table (see the read-scope guard test).
pub const EEG_QUERY: &str =
    "SELECT wall_start, focus FROM eeg_timeseries WHERE wall_start >= ?1 AND wall_start < ?2 ORDER BY wall_start";

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

/// Mean `focus` (0..100) across EEG epochs in `[start, end)`. `None` if there are
/// no epochs in the window. Read-only.
pub fn mean_focus(activity_db: &Path, start: i64, end: i64) -> Result<Option<f64>, String> {
    let conn = open_ro(activity_db)?;
    let mut stmt = conn
        .prepare(EEG_QUERY)
        .map_err(|e| format!("NeuroSkill eeg query failed: {e}"))?;
    let rows = stmt
        .query_map([start, end], |row| row.get::<_, f64>(1))
        .map_err(|e| format!("NeuroSkill eeg read failed: {e}"))?;
    let mut sum = 0.0;
    let mut n = 0u32;
    for r in rows {
        sum += r.map_err(|e| format!("NeuroSkill eeg row failed: {e}"))?;
        n += 1;
    }
    Ok((n > 0).then(|| sum / n as f64))
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
