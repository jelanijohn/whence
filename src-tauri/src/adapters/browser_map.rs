//! Browser-adapter **mapping store** (§4) — the daemon-owned project registry for
//! browser LLM sessions.
//!
//! Browser-minted projects are *rootless* (no filesystem path), so they can't live
//! in the `project_aliases` map (which is keyed on the Claude Code transcript dir
//! name). This is their parallel keyspace. The two unify at the **slug** layer, not
//! the alias layer (§9): a browser project and an fs project with the same name
//! converge because both slugs come from the shared [`slugify`](super::slugify).
//!
//! Each record maps a provider's own project identity to a Whence slug, with two
//! lookup keys:
//!   * **Level-1** = a normalized project-scoped URL (`urls`), the fast path. Appended
//!     to (backfilled) as new URLs are seen, so the entry generalizes across every
//!     conversation in a project.
//!   * **Level-2** = `(provider, provider_project_id)`, the stable identity. Survives a
//!     project rename, so attribution doesn't break when the readable name changes.
//!
//! ```toml
//! [[project]]
//! slug                  = "whence"
//! provider              = "claude"
//! provider_project_id   = "…"        # Level-2 key — survives rename
//! provider_project_name = "Whence"   # last-seen readable name; source of the slug
//! urls                  = ["https://claude.ai/project/…"]  # Level-1 fast-path keys
//! ```
//!
//! **Format-preserving.** Backed by `toml_edit`, so the daemon's writes (backfill,
//! mint) round-trip without clobbering the user's hand-edits, comments, or ordering —
//! the file is meant to be hand-editable (re-slug a rename, fix a bad mint). A
//! present-but-unparseable file is an error, never a silent overwrite.

use std::path::{Path, PathBuf};

use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};

use super::slugify;

/// The array-of-tables key holding the project records.
const PROJECT: &str = "project";

/// `<data_dir>/browser_mapping.toml` — the on-disk mapping store. Surfaced in
/// settings via a deep-link so it's easy to hand-edit (§7).
pub fn mapping_path(data_dir: &Path) -> PathBuf {
    data_dir.join("browser_mapping.toml")
}

/// The mapping store: a format-preserving TOML document plus the path it loads
/// from. An in-memory store (no path) never persists — used by tests and as a safe
/// fallback when the data dir is unavailable.
pub struct MappingStore {
    /// `None` = in-memory only (tests / fallback); writes are no-ops.
    path: Option<PathBuf>,
    doc: DocumentMut,
}

impl MappingStore {
    /// Parse from a TOML string, in-memory (tests). Errors on invalid TOML.
    #[cfg(test)]
    pub fn from_str(s: &str) -> Result<Self, String> {
        let doc = s
            .parse::<DocumentMut>()
            .map_err(|e| format!("invalid mapping TOML: {e}"))?;
        Ok(Self { path: None, doc })
    }

    /// Load from disk, defaulting to an empty store when the file is missing (first
    /// run). A present-but-unparseable file is an error rather than a silent
    /// overwrite — we never clobber a mapping we can't understand.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let doc = s
                    .parse::<DocumentMut>()
                    .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
                Ok(Self { path: Some(path.to_path_buf()), doc })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                path: Some(path.to_path_buf()),
                doc: DocumentMut::new(),
            }),
            Err(e) => Err(format!("could not read {}: {e}", path.display())),
        }
    }

    /// **Level-1** lookup: a normalized project-scoped URL already on record → its
    /// slug. The fast path — no DOM extraction needed when it hits.
    pub fn lookup_url(&self, normalized_url: &str) -> Option<String> {
        for t in self.tables() {
            let on_record = t
                .get("urls")
                .and_then(Item::as_array)
                .map(|urls| urls.iter().any(|v| v.as_str() == Some(normalized_url)))
                .unwrap_or(false);
            if on_record {
                return t.get("slug").and_then(Item::as_str).map(str::to_string);
            }
        }
        None
    }

    /// **Level-2** lookup: a `(provider, provider_project_id)` already on record → its
    /// slug. Stable across renames (keyed on the opaque id, not the name).
    pub fn lookup_provider(&self, provider: &str, id: &str) -> Option<String> {
        let idx = self.find_provider_index(provider, id)?;
        self.tables().nth(idx)?.get("slug").and_then(Item::as_str).map(str::to_string)
    }

    /// Append `url` to an existing record's Level-1 keys (the backfill on a Level-2
    /// hit), so the next visit to this URL takes the fast path. Idempotent — a URL
    /// already on record is a no-op (and no write). Best-effort persistence: a write
    /// failure is swallowed (attribution already succeeded; the URL just isn't cached).
    pub fn backfill_url(&mut self, provider: &str, id: &str, url: &str) {
        let Some(idx) = self.find_provider_index(provider, id) else { return };
        let mut changed = false;
        if let Some(aot) = self.aot_mut() {
            if let Some(t) = aot.get_mut(idx) {
                let urls = t.entry("urls").or_insert_with(|| value(Array::new()));
                if let Some(arr) = urls.as_array_mut() {
                    if !arr.iter().any(|v| v.as_str() == Some(url)) {
                        arr.push(url);
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.save();
        }
    }

    /// **Mint** a new project from the provider's readable name: `slug = slugify(name)`
    /// (§5), the same primitive the fs resolver uses — so a browser-minted project and
    /// an fs-resolved one with the same name converge onto one slug. Registers the
    /// record (with `id` as the Level-2 key) and persists.
    ///
    /// Returns the slug even if registration/persistence fails: the slug is a pure
    /// function of the name, so a failed write just means we re-mint the *same* slug
    /// next time — attribution never breaks because the file is weird. `None` only
    /// when the name slugs to nothing.
    pub fn mint(&mut self, provider: &str, id: &str, name: &str) -> Option<String> {
        let slug = slugify(name)?;
        if let Some(aot) = self.aot_mut() {
            let mut t = Table::new();
            t["slug"] = value(slug.as_str());
            t["provider"] = value(provider);
            t["provider_project_id"] = value(id);
            t["provider_project_name"] = value(name);
            // Empty Level-1 list; filled by backfill_url once a URL is observed.
            t["urls"] = value(Array::new());
            aot.push(t);
            self.save();
        }
        Some(slug)
    }

    // --- internals ----------------------------------------------------------

    /// Iterate the project records (read-only).
    fn tables(&self) -> impl Iterator<Item = &Table> {
        self.doc
            .get(PROJECT)
            .and_then(Item::as_array_of_tables)
            .into_iter()
            .flat_map(|aot| aot.iter())
    }

    /// Index of the record matching `(provider, id)`, if any.
    fn find_provider_index(&self, provider: &str, id: &str) -> Option<usize> {
        self.tables().position(|t| {
            t.get("provider").and_then(Item::as_str) == Some(provider)
                && t.get("provider_project_id").and_then(Item::as_str) == Some(id)
        })
    }

    /// The project array-of-tables, mutably, creating it if absent. `None` only when
    /// the file has a `project` key that *isn't* an array-of-tables (user-corrupted) —
    /// in which case callers skip the write rather than fight the user's shape.
    fn aot_mut(&mut self) -> Option<&mut ArrayOfTables> {
        self.doc
            .entry(PROJECT)
            .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
    }

    /// Persist the document, best-effort. In-memory stores (no path) are a no-op; a
    /// write error is logged, never propagated (attribution must not depend on it).
    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("whence: could not create {}: {e}", parent.display());
                return;
            }
        }
        if let Err(e) = std::fs::write(path, self.doc.to_string()) {
            eprintln!("whence: browser mapping not saved to {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: &str = r#"# my browser projects — hand-editable
[[project]]
slug                  = "whence"
provider              = "claude"
provider_project_id   = "proj_abc"
provider_project_name = "Whence"
urls                  = ["https://claude.ai/project/proj_abc"]
"#;

    #[test]
    fn level1_url_lookup_hits() {
        let store = MappingStore::from_str(SEED).unwrap();
        assert_eq!(
            store.lookup_url("https://claude.ai/project/proj_abc").as_deref(),
            Some("whence")
        );
        // An unknown URL misses (the daemon then falls through to DOM extraction).
        assert!(store.lookup_url("https://claude.ai/project/other").is_none());
    }

    #[test]
    fn level2_provider_id_lookup_survives_rename() {
        let store = MappingStore::from_str(SEED).unwrap();
        // The id is the stable key — the readable name is irrelevant to the lookup.
        assert_eq!(store.lookup_provider("claude", "proj_abc").as_deref(), Some("whence"));
        assert!(store.lookup_provider("claude", "nope").is_none());
        assert!(store.lookup_provider("chatgpt", "proj_abc").is_none());
    }

    #[test]
    fn backfill_appends_url_and_preserves_comments() {
        let mut store = MappingStore::from_str(SEED).unwrap();
        let new_url = "https://claude.ai/project/proj_abc/conversation/xyz";
        store.backfill_url("claude", "proj_abc", new_url);
        // Now a fast-path hit.
        assert_eq!(store.lookup_url(new_url).as_deref(), Some("whence"));
        let out = store.doc.to_string();
        assert!(out.contains(new_url));
        // The hand-written comment and the original URL survive the write (§4).
        assert!(out.contains("# my browser projects — hand-editable"));
        assert!(out.contains("https://claude.ai/project/proj_abc\""));
    }

    #[test]
    fn backfill_is_idempotent() {
        let mut store = MappingStore::from_str(SEED).unwrap();
        let url = "https://claude.ai/project/proj_abc";
        store.backfill_url("claude", "proj_abc", url); // already on record
        let count = store.doc.to_string().matches(url).count();
        assert_eq!(count, 1, "an existing URL must not be appended twice");
    }

    #[test]
    fn mint_slugifies_name_and_registers_record() {
        let mut store = MappingStore::from_str(SEED).unwrap();
        // A spaced, cased name slugs canonically (and converges with an fs "one-domino-square").
        let slug = store.mint("chatgpt", "g-123", "One Domino Square").unwrap();
        assert_eq!(slug, "one-domino-square");
        // The new record is now resolvable by its Level-2 key.
        assert_eq!(store.lookup_provider("chatgpt", "g-123").as_deref(), Some("one-domino-square"));
        // The pre-existing record is untouched.
        assert_eq!(store.lookup_provider("claude", "proj_abc").as_deref(), Some("whence"));
    }

    #[test]
    fn mint_from_empty_store() {
        let mut store = MappingStore::from_str("").unwrap();
        let slug = store.mint("claude", "p1", "Whence").unwrap();
        assert_eq!(slug, "whence");
        assert_eq!(store.lookup_provider("claude", "p1").as_deref(), Some("whence"));
        // A name that slugs to nothing can't mint.
        assert!(store.mint("claude", "p2", "  !!  ").is_none());
    }

    #[test]
    fn missing_file_loads_empty_not_error() {
        let path = std::env::temp_dir().join("whence-no-such-mapping-xyz.toml");
        std::fs::remove_file(&path).ok();
        let store = MappingStore::load(&path).unwrap();
        assert!(store.lookup_provider("claude", "anything").is_none());
    }
}
