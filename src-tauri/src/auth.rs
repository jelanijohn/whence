//! Receiver auth — the bearer token gating Whence's loopback listeners.
//!
//! The three receivers (Claude hooks `18450`, terminal cwd `18451`, browser
//! `18452`) accept POSTs that feed the attribution engine — and Whence's output
//! is ground-truth labels for EEG data, so *any* local process (or, depending on
//! the browser's private-network-access enforcement, a drive-by web page firing a
//! no-CORS POST at localhost) being able to forge a confidence-1.0 `Prompt` is a
//! data-integrity hole, not a theoretical one. The credential is the gate —
//! socket/credential separation beats trying to infer callers from Origin or
//! forwarded headers.
//!
//! One **per-install** token (not per-session: the hook URL is written once into
//! `.claude/settings.json` and must survive restarts), minted at first launch,
//! stored `0600` in the app data dir, shown and rotatable in Settings. Callers
//! present it either way:
//!   * `Authorization: Bearer <token>` — the terminal snippet and the extension.
//!   * a trailing `/<token>` path segment — Claude Code `http` hooks can't set
//!     headers, so `install_claude_hooks` embeds it in the URL.
//!
//! Denied requests get a 401 and increment a visible counter (a Settings
//! diagnostic — Whence reports, it never scolds). Layering mirrors the adapters:
//! [`request_authorized`] is **pure and fixture-tested**; the file I/O and the
//! `tiny_http` header extraction are the thin impure shell around it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// The live token, shared between the receiver threads (readers) and the rotate
/// command (writer) — so a rotation applies immediately, no restart.
pub type SharedToken = Arc<RwLock<String>>;

/// Total denied (401) requests across all receivers since launch. In-memory only:
/// it's a diagnostic ("something local is poking the ports"), not a log.
pub type Denials = Arc<AtomicU64>;

/// Token file name in the app data dir. A neighbor of `settings.json`, but its
/// own file so it can carry `0600` and rotate without touching settings.
const TOKEN_FILE: &str = "receiver.token";

pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TOKEN_FILE)
}

/// Load the persisted token, minting (and best-effort persisting) one on first
/// launch. Infallible by design: if the disk write fails the freshly minted
/// token still gates this run (the hook installer reads the live value, so
/// everything stays consistent) — a sensor must not refuse to start over a
/// persistence hiccup.
pub fn load_or_mint(data_dir: &Path) -> String {
    let path = token_path(data_dir);
    if let Ok(s) = std::fs::read_to_string(&path) {
        let existing = s.trim().to_string();
        if !existing.is_empty() {
            return existing;
        }
    }
    let token = mint_token();
    if let Err(e) = persist(&path, &token) {
        eprintln!("whence: could not persist receiver token to {}: {e}", path.display());
    }
    token
}

/// Mint and persist a fresh token (the Settings "rotate" action). Unlike first
/// launch, a failed write here is an error — the user asked for a rotation and
/// must know it didn't stick.
pub fn rotate(data_dir: &Path) -> std::io::Result<String> {
    let token = mint_token();
    persist(&token_path(data_dir), &token)?;
    Ok(token)
}

/// 32 bytes of OS randomness, hex-encoded (64 chars — URL- and header-safe).
fn mint_token() -> String {
    let mut buf = [0u8; 32];
    if getrandom::fill(&mut buf).is_err() {
        // OS RNG unavailable (should never happen on a desktop target). Fall back
        // to a clock/pid hash chain — weak, but strictly better than no token,
        // and loud about it.
        eprintln!("whence: OS randomness unavailable; minting a weak receiver token");
        use std::hash::{Hash, Hasher};
        let mut seed = std::collections::hash_map::DefaultHasher::new();
        std::process::id().hash(&mut seed);
        std::time::SystemTime::now().hash(&mut seed);
        for chunk in buf.chunks_mut(8) {
            let h = seed.finish();
            chunk.copy_from_slice(&h.to_le_bytes()[..chunk.len()]);
            h.hash(&mut seed);
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Write the token `0600` (owner-only) on Unix; on Windows the app data dir is
/// already per-user under the profile ACLs. `set_permissions` after the write
/// also tightens a pre-existing file that was created looser.
fn persist(path: &Path, token: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The pure gate: is a request carrying `auth_header` (the `Authorization` value,
/// if any) and hitting `url_path` (path + optional query, as `tiny_http` reports
/// it) authorized against `token`?
///
/// Accepts a `Bearer <token>` header or a trailing `/<token>` path segment (the
/// Claude Code `http`-hook carrier). An empty token authorizes nothing — fail
/// closed if minting ever produced one.
pub fn request_authorized(auth_header: Option<&str>, url_path: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    if let Some(header) = auth_header {
        if let Some(presented) = header.trim().strip_prefix("Bearer ") {
            if constant_time_eq(presented.trim(), token) {
                return true;
            }
        }
    }
    let path = url_path.split(['?', '#']).next().unwrap_or(url_path);
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    constant_time_eq(last, token)
}

/// Byte-wise comparison without an early exit, so a loopback prober can't binary-
/// search the token by response timing. Length is not secret (it's always 64).
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The impure shim over [`request_authorized`] for a `tiny_http` request: pull
/// the `Authorization` header (field names are case-insensitive) and the URL.
pub fn tiny_http_authorized(req: &tiny_http::Request, token: &str) -> bool {
    let header = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .map(|h| h.value.as_str().to_string());
    request_authorized(header.as_deref(), req.url(), token)
}

/// Count one denied request. Logging is capped (first few, then every 100th) so
/// a chatty prober can't turn stderr into its own denial-of-service.
pub fn record_denial(denials: &Denials, receiver: &str) {
    let n = denials.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 5 || n % 100 == 0 {
        eprintln!("whence: rejected unauthenticated request to the {receiver} receiver ({n} total)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "d00d8e6c0f1e4b5a9c3f2e1d0b9a8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b";

    #[test]
    fn bearer_header_authorizes() {
        let h = format!("Bearer {TOKEN}");
        assert!(request_authorized(Some(&h), "/hook", TOKEN));
        // Surrounding whitespace is tolerated; the scheme word is not optional.
        let padded = format!("  Bearer {TOKEN}  ");
        assert!(request_authorized(Some(&padded), "/cwd", TOKEN));
        assert!(!request_authorized(Some(TOKEN), "/cwd", TOKEN));
    }

    #[test]
    fn path_token_authorizes() {
        assert!(request_authorized(None, &format!("/hook/{TOKEN}"), TOKEN));
        // Query string and trailing slash don't break the segment read.
        assert!(request_authorized(None, &format!("/hook/{TOKEN}?x=1"), TOKEN));
        assert!(request_authorized(None, &format!("/hook/{TOKEN}/"), TOKEN));
    }

    #[test]
    fn wrong_or_missing_credentials_deny() {
        assert!(!request_authorized(None, "/hook", TOKEN));
        assert!(!request_authorized(Some("Bearer nope"), "/hook", TOKEN));
        assert!(!request_authorized(None, "/hook/nope", TOKEN));
        // A token that merely prefixes/suffixes the real one is not the token.
        assert!(!request_authorized(None, &format!("/hook/{TOKEN}x"), TOKEN));
        let truncated = &TOKEN[..TOKEN.len() - 1];
        assert!(!request_authorized(None, &format!("/hook/{truncated}"), TOKEN));
    }

    #[test]
    fn empty_token_fails_closed() {
        // If the stored token were ever empty, nothing authorizes — not even the
        // degenerate "empty last segment" of a bare path.
        assert!(!request_authorized(None, "/hook/", ""));
        assert!(!request_authorized(Some("Bearer "), "/hook", ""));
    }

    #[test]
    fn minted_tokens_are_distinct_hex() {
        let a = mint_token();
        let b = mint_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn load_persists_then_reloads_same_token() {
        let dir = std::env::temp_dir().join("whence-auth-test-load");
        std::fs::remove_dir_all(&dir).ok();
        let first = load_or_mint(&dir);
        let second = load_or_mint(&dir);
        assert_eq!(first, second, "reload must return the persisted token");
        let rotated = rotate(&dir).unwrap();
        assert_ne!(rotated, first);
        assert_eq!(load_or_mint(&dir), rotated);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("whence-auth-test-perms");
        std::fs::remove_dir_all(&dir).ok();
        load_or_mint(&dir);
        let mode = std::fs::metadata(token_path(&dir)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "token file must be 0600");
        std::fs::remove_dir_all(&dir).ok();
    }
}
