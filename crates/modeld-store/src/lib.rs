//! Canonical content-addressed model store: `~/.modeld/{blobs,registry.db}`.
//!
//! The store owns one physical copy per unique artifact. Blobs are APFS clones of
//! provider files (`blobs/sha256-<hex>`, zero marginal disk cost on import), so the
//! store can anchor bytes even after every provider deletes its copy. The `SQLite`
//! registry records artifacts, which provider paths reference them, and a digest
//! cache keyed on `(path, size, mtime)` so re-syncs never re-hash settled files.
//!
//! Callers must only import digests they have verified by hashing the source file —
//! the store trusts its inputs and verifies nothing itself (single hashing site
//! lives with the caller, which also owns progress reporting).

use modeld_core::{Digest, Format, ProviderKind};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("registry error: {0}")]
    Registry(#[from] rusqlite::Error),
    #[error("store I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of importing one blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    /// Blob cloned into the store and registered.
    Imported,
    /// Blob already present; registry row refreshed.
    AlreadyPresent,
    /// Source cannot be cloned into the store volume (cross-volume or non-APFS).
    NotCloneable,
}

/// One provider path referencing a stored artifact.
#[derive(Debug, Clone)]
pub struct Reference {
    pub path: PathBuf,
    pub provider: String,
    pub label: Option<String>,
}

/// A stored artifact with everything referencing it (for `ls`/`where`).
#[derive(Debug, Clone)]
pub struct StoredArtifact {
    pub digest: Digest,
    pub size: u64,
    pub format: Option<String>,
    pub references: Vec<Reference>,
}

/// Aggregate storage accounting for the `ls` footer.
#[derive(Debug, Clone, Copy, Default)]
pub struct Totals {
    /// One copy per unique artifact (what disk actually holds after dedupe).
    pub physical_bytes: u64,
    /// Sum over all provider references (what disk would hold without sharing).
    pub logical_bytes: u64,
}

/// Handle to the store directory and its registry.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    conn: Connection,
}

impl Store {
    /// Opens (creating if needed) the store at `root`, typically `~/.modeld`.
    ///
    /// # Errors
    /// I/O error creating directories, or `SQLite` error opening/migrating the
    /// registry.
    pub fn open(root: PathBuf) -> Result<Self, StoreError> {
        std::fs::create_dir_all(root.join("blobs"))?;
        let conn = Connection::open(root.join("registry.db"))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS artifacts (
                 digest      TEXT PRIMARY KEY,
                 size        INTEGER NOT NULL,
                 format      TEXT,
                 imported_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS refs (
                 path      TEXT PRIMARY KEY,
                 digest    TEXT NOT NULL,
                 provider  TEXT NOT NULL,
                 label     TEXT,
                 last_seen INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS refs_by_digest ON refs(digest);
             CREATE TABLE IF NOT EXISTS digest_cache (
                 path        TEXT PRIMARY KEY,
                 size        INTEGER NOT NULL,
                 mtime_secs  INTEGER NOT NULL,
                 mtime_nanos INTEGER NOT NULL,
                 digest      TEXT NOT NULL
             );",
        )?;
        Ok(Self { root, conn })
    }

    /// Path a blob for `digest` lives at (whether or not it exists yet).
    #[must_use]
    pub fn blob_path(&self, digest: &Digest) -> PathBuf {
        self.root.join("blobs").join(format!(
            "{}-{}",
            digest.algorithm().as_str(),
            digest.to_hex()
        ))
    }

    /// Imports `source` as the canonical blob for `digest` (caller-verified).
    ///
    /// Cloning is copy-on-write: zero marginal disk cost, and later mutation of
    /// `source` cannot corrupt the stored blob.
    ///
    /// # Errors
    /// Registry failure, or I/O failure other than the clone being impossible
    /// (cross-volume clones report [`ImportOutcome::NotCloneable`] instead).
    pub fn import_blob(
        &self,
        source: &Path,
        digest: &Digest,
        size: u64,
        format: Option<&Format>,
    ) -> Result<ImportOutcome, StoreError> {
        let blob = self.blob_path(digest);
        let outcome = if blob.exists() {
            ImportOutcome::AlreadyPresent
        } else {
            match modeld_core::apfs::clone_file(source, &blob) {
                Ok(()) => ImportOutcome::Imported,
                Err(error) if matches!(error.raw_os_error(), Some(libc::EXDEV | libc::ENOTSUP)) => {
                    return Ok(ImportOutcome::NotCloneable);
                }
                Err(error) => return Err(error.into()),
            }
        };
        self.conn.execute(
            "INSERT INTO artifacts (digest, size, format, imported_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(digest) DO UPDATE SET size = ?2, format = ?3",
            params![
                digest.to_string(),
                size,
                format.map(format_name),
                epoch_secs()
            ],
        )?;
        Ok(outcome)
    }

    /// Records that `path` (owned by `provider`) references `digest`.
    ///
    /// # Errors
    /// Registry failure.
    pub fn record_reference(
        &self,
        digest: &Digest,
        path: &Path,
        provider: ProviderKind,
        label: Option<&str>,
        seen_at: u64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO refs (path, digest, provider, label, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET
                 digest = ?2, provider = ?3, label = ?4, last_seen = ?5",
            params![
                path.to_string_lossy(),
                digest.to_string(),
                provider_name(provider),
                label,
                seen_at
            ],
        )?;
        Ok(())
    }

    /// Drops references not seen since `stamp` (files deleted or moved away).
    ///
    /// # Errors
    /// Registry failure.
    pub fn prune_references_before(&self, stamp: u64) -> Result<usize, StoreError> {
        let dropped = self
            .conn
            .execute("DELETE FROM refs WHERE last_seen < ?1", params![stamp])?;
        Ok(dropped)
    }

    /// Looks up a previously computed digest for an unchanged file.
    ///
    /// # Errors
    /// Registry failure.
    pub fn cached_digest(
        &self,
        path: &Path,
        size: u64,
        mtime_secs: u64,
        mtime_nanos: u32,
    ) -> Result<Option<Digest>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT digest FROM digest_cache
             WHERE path = ?1 AND size = ?2 AND mtime_secs = ?3 AND mtime_nanos = ?4",
        )?;
        let digest = statement
            .query_row(
                params![path.to_string_lossy(), size, mtime_secs, mtime_nanos],
                |row| row.get::<_, String>(0),
            )
            .map(|text| text.parse().ok())
            .map_or_else(
                |error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                },
                Ok,
            )?;
        Ok(digest)
    }

    /// Remembers a computed digest for `(path, size, mtime)`.
    ///
    /// # Errors
    /// Registry failure.
    pub fn remember_digest(
        &self,
        path: &Path,
        size: u64,
        mtime_secs: u64,
        mtime_nanos: u32,
        digest: &Digest,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO digest_cache (path, size, mtime_secs, mtime_nanos, digest)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET
                 size = ?2, mtime_secs = ?3, mtime_nanos = ?4, digest = ?5",
            params![
                path.to_string_lossy(),
                size,
                mtime_secs,
                mtime_nanos,
                digest.to_string()
            ],
        )?;
        Ok(())
    }

    /// All stored artifacts with their references, largest first (for `ls`).
    ///
    /// # Errors
    /// Registry failure, or a corrupt digest in the registry.
    pub fn artifacts(&self) -> Result<Vec<StoredArtifact>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT digest, size, format FROM artifacts ORDER BY size DESC")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut artifacts = Vec::with_capacity(rows.len());
        for (digest_text, size, format) in rows {
            let Ok(digest) = digest_text.parse::<Digest>() else {
                continue;
            };
            let references = self.references_for(&digest_text)?;
            artifacts.push(StoredArtifact {
                digest,
                size,
                format,
                references,
            });
        }
        Ok(artifacts)
    }

    /// Stored artifacts whose label or path contains `query` (case-insensitive).
    ///
    /// # Errors
    /// Registry failure.
    pub fn find(&self, query: &str) -> Result<Vec<StoredArtifact>, StoreError> {
        let needle = query.to_lowercase();
        Ok(self
            .artifacts()?
            .into_iter()
            .filter(|artifact| {
                artifact.references.iter().any(|reference| {
                    reference
                        .label
                        .as_deref()
                        .is_some_and(|label| label.to_lowercase().contains(&needle))
                        || reference
                            .path
                            .to_string_lossy()
                            .to_lowercase()
                            .contains(&needle)
                })
            })
            .collect())
    }

    /// Physical vs logical storage accounting across all references.
    ///
    /// # Errors
    /// Registry failure.
    pub fn totals(&self) -> Result<Totals, StoreError> {
        let physical =
            self.conn
                .query_row("SELECT COALESCE(SUM(size), 0) FROM artifacts", [], |row| {
                    row.get::<_, u64>(0)
                })?;
        let logical = self.conn.query_row(
            "SELECT COALESCE(SUM(a.size), 0) FROM refs r JOIN artifacts a ON a.digest = r.digest",
            [],
            |row| row.get::<_, u64>(0),
        )?;
        Ok(Totals {
            physical_bytes: physical,
            logical_bytes: logical,
        })
    }

    fn references_for(&self, digest_text: &str) -> Result<Vec<Reference>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT path, provider, label FROM refs WHERE digest = ?1 ORDER BY path")?;
        let references = statement
            .query_map(params![digest_text], |row| {
                Ok(Reference {
                    path: PathBuf::from(row.get::<_, String>(0)?),
                    provider: row.get(1)?,
                    label: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(references)
    }
}

/// Current wall-clock as epoch seconds (sync stamps, import timestamps).
#[must_use]
pub fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn provider_name(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Ollama => "ollama",
        ProviderKind::HuggingFace => "huggingface",
        ProviderKind::LmStudio => "lmstudio",
        _ => "manual",
    }
}

fn format_name(format: &Format) -> String {
    match format {
        Format::Gguf => "GGUF".to_string(),
        Format::Safetensors => "safetensors".to_string(),
        Format::Onnx => "ONNX".to_string(),
        Format::Other(name) => name.clone(),
        _ => "other".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modeld_core::Algorithm;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = Store::open(dir.path().join(".modeld")).expect("open store");
        (dir, store)
    }

    fn digest_of(content: &[u8], dir: &Path) -> (PathBuf, Digest) {
        let path = dir.join("source.gguf");
        std::fs::write(&path, content).expect("write source");
        let digest = Digest::sha256_file(&path).expect("hash");
        (path, digest)
    }

    #[test]
    fn import_clones_blob_once_and_is_idempotent() {
        let (dir, store) = store();
        let (source, digest) = digest_of(b"weights", dir.path());

        let first = store
            .import_blob(&source, &digest, 7, Some(&Format::Gguf))
            .expect("import");
        let second = store
            .import_blob(&source, &digest, 7, Some(&Format::Gguf))
            .expect("re-import");

        assert_eq!(first, ImportOutcome::Imported);
        assert_eq!(second, ImportOutcome::AlreadyPresent);
        assert_eq!(
            std::fs::read(store.blob_path(&digest)).expect("read blob"),
            b"weights"
        );
    }

    #[test]
    fn references_aggregate_under_their_artifact() {
        let (dir, store) = store();
        let (source, digest) = digest_of(b"weights", dir.path());
        store
            .import_blob(&source, &digest, 7, Some(&Format::Gguf))
            .expect("import");
        store
            .record_reference(
                &digest,
                &source,
                ProviderKind::LmStudio,
                Some("repo/model"),
                100,
            )
            .expect("ref 1");
        store
            .record_reference(
                &digest,
                &dir.path().join("elsewhere.gguf"),
                ProviderKind::HuggingFace,
                None,
                100,
            )
            .expect("ref 2");

        let artifacts = store.artifacts().expect("artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].references.len(), 2);
        let totals = store.totals().expect("totals");
        assert_eq!(totals.physical_bytes, 7);
        assert_eq!(totals.logical_bytes, 14);
    }

    #[test]
    fn prune_drops_references_not_seen_this_sync() {
        let (dir, store) = store();
        let (source, digest) = digest_of(b"weights", dir.path());
        store
            .import_blob(&source, &digest, 7, None)
            .expect("import");
        store
            .record_reference(&digest, &source, ProviderKind::LmStudio, None, 100)
            .expect("ref");

        let dropped = store.prune_references_before(101).expect("prune");

        assert_eq!(dropped, 1);
        assert!(
            store.artifacts().expect("artifacts")[0]
                .references
                .is_empty()
        );
    }

    #[test]
    fn digest_cache_hits_only_on_unchanged_stat() {
        let (dir, store) = store();
        let path = dir.path().join("file");
        let digest = Digest::new(Algorithm::Sha256, vec![9; 32]).expect("digest");
        store
            .remember_digest(&path, 10, 1000, 500, &digest)
            .expect("remember");

        let hit = store.cached_digest(&path, 10, 1000, 500).expect("query");
        let stale = store.cached_digest(&path, 10, 1000, 501).expect("query");

        assert_eq!(hit, Some(digest));
        assert_eq!(stale, None);
    }

    #[test]
    fn find_matches_labels_case_insensitively() {
        let (dir, store) = store();
        let (source, digest) = digest_of(b"weights", dir.path());
        store
            .import_blob(&source, &digest, 7, None)
            .expect("import");
        store
            .record_reference(
                &digest,
                &source,
                ProviderKind::Ollama,
                Some("ollama/qwen3.5:4b"),
                1,
            )
            .expect("ref");

        assert_eq!(store.find("QWEN3").expect("find").len(), 1);
        assert!(store.find("gemma").expect("find").is_empty());
    }
}
