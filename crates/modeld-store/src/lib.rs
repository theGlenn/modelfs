//! Canonical content-addressed model store: `~/.modeld/{blobs,registry.db}`.
//!
//! The store owns one physical copy per unique artifact. Blobs are APFS clones of
//! provider files (`blobs/sha256-<hex>`, zero marginal disk cost on import), so the
//! store can anchor bytes even after every provider deletes its copy. The `SQLite`
//! registry records artifacts, which provider paths reference them, and a digest
//! cache keyed on full file identity and timestamps so re-syncs never re-hash
//! settled files.
//!
//! Callers must only import digests they have verified by hashing the source file —
//! the store trusts its inputs and verifies nothing itself (single hashing site
//! lives with the caller, which also owns progress reporting).

use modeld_core::{Digest, Format, ProviderKind};
use rusqlite::{Connection, params};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("registry error: {0}")]
    Registry(#[from] rusqlite::Error),
    #[error("store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "canonical blob {} has digest {actual}, expected {expected}",
        path.display()
    )]
    BlobDigestMismatch {
        path: PathBuf,
        expected: Digest,
        actual: Digest,
    },
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
    pub semantics: Option<Semantics>,
}

/// Display-oriented facts read from the artifact's own header at sync time.
///
/// `kind` doubles as the analyzed marker: a row with no semantics has never
/// been inspected and will be on the next sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Semantics {
    /// `model` for weights, `asset` for tokenizers/vocabularies and similar.
    pub kind: String,
    pub name: Option<String>,
    pub architecture: Option<String>,
    pub quant: Option<String>,
    pub params: Option<String>,
}

/// Aggregate storage accounting for the `ls` footer.
#[derive(Debug, Clone, Copy, Default)]
pub struct Totals {
    /// One copy per unique artifact (what disk actually holds after dedupe).
    pub physical_bytes: u64,
    /// Sum over all provider references (what disk would hold without sharing).
    pub logical_bytes: u64,
}

/// Filesystem identity used to validate cached digests and known APFS clones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
    pub ctime_secs: u64,
    pub ctime_nanos: u32,
}

impl FileStamp {
    /// Captures all metadata that changes when a file is replaced or written.
    #[must_use]
    pub fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            mtime_secs: u64::try_from(metadata.mtime()).unwrap_or(0),
            mtime_nanos: u32::try_from(metadata.mtime_nsec()).unwrap_or(0),
            ctime_secs: u64::try_from(metadata.ctime()).unwrap_or(0),
            ctime_nanos: u32::try_from(metadata.ctime_nsec()).unwrap_or(0),
        }
    }

    /// Stats `path` and captures its current stamp.
    ///
    /// # Errors
    /// Returns the underlying stat error.
    pub fn of(path: &Path) -> std::io::Result<Self> {
        std::fs::metadata(path).map(|metadata| Self::from_metadata(&metadata))
    }
}

/// Handle to the store directory and its registry.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    conn: Connection,
    /// Avoid re-hashing the same multi-gigabyte canonical once per reference in
    /// a single command while still validating it at least once per store open.
    verified_blobs: RefCell<HashSet<Digest>>,
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
                 device      INTEGER NOT NULL,
                 inode       INTEGER NOT NULL,
                 size        INTEGER NOT NULL,
                 mtime_secs  INTEGER NOT NULL,
                 mtime_nanos INTEGER NOT NULL,
                 ctime_secs  INTEGER NOT NULL,
                 ctime_nanos INTEGER NOT NULL,
                 digest      TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS shared_paths (
                 path        TEXT PRIMARY KEY,
                 digest      TEXT NOT NULL,
                 device      INTEGER NOT NULL,
                 inode       INTEGER NOT NULL,
                 size        INTEGER NOT NULL,
                 mtime_secs  INTEGER NOT NULL,
                 mtime_nanos INTEGER NOT NULL,
                 ctime_secs  INTEGER NOT NULL,
                 ctime_nanos INTEGER NOT NULL
             );",
        )?;
        migrate_stamp_columns(&conn, "digest_cache")?;
        migrate_semantics_columns(&conn)?;
        Ok(Self {
            root,
            conn,
            verified_blobs: RefCell::new(HashSet::new()),
        })
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
        format: Option<&Format>,
    ) -> Result<ImportOutcome, StoreError> {
        let blob = self.blob_path(digest);
        let already_verified = self.verified_blobs.borrow().contains(digest);
        let mut outcome = ImportOutcome::AlreadyPresent;

        if !already_verified {
            if blob.exists() {
                let actual = Digest::sha256_file(&blob)?;
                if actual != *digest {
                    std::fs::remove_file(&blob)?;
                    outcome = self.clone_verified(source, &blob, digest)?;
                }
            } else {
                outcome = self.clone_verified(source, &blob, digest)?;
            }
            if outcome == ImportOutcome::NotCloneable {
                return Ok(outcome);
            }
            self.verified_blobs.borrow_mut().insert(digest.clone());
        }

        let verified_size = std::fs::metadata(&blob)?.len();
        self.conn.execute(
            "INSERT INTO artifacts (digest, size, format, imported_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(digest) DO UPDATE SET size = ?2, format = ?3",
            params![
                digest.to_string(),
                verified_size,
                format.map(format_name),
                epoch_secs()
            ],
        )?;
        Ok(outcome)
    }

    fn clone_verified(
        &self,
        source: &Path,
        blob: &Path,
        expected: &Digest,
    ) -> Result<ImportOutcome, StoreError> {
        match modeld_core::apfs::clone_file(source, blob) {
            Ok(()) => {}
            Err(error) if matches!(error.raw_os_error(), Some(libc::EXDEV | libc::ENOTSUP)) => {
                return Ok(ImportOutcome::NotCloneable);
            }
            Err(error) => return Err(error.into()),
        }
        let source_stamp = FileStamp::of(source)?;
        let actual = Digest::sha256_file(blob)?;
        if actual != *expected {
            let _ = std::fs::remove_file(blob);
            return Err(StoreError::BlobDigestMismatch {
                path: blob.to_path_buf(),
                expected: expected.clone(),
                actual,
            });
        }
        self.record_shared_stamp(expected, source, source_stamp)?;
        Ok(ImportOutcome::Imported)
    }

    /// Whether the artifact for `digest` still awaits header inspection.
    ///
    /// Returns false for unknown digests — there is no row to attach facts to.
    ///
    /// # Errors
    /// Registry failure.
    pub fn semantics_pending(&self, digest: &Digest) -> Result<bool, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT kind IS NULL FROM artifacts WHERE digest = ?1")?;
        match statement.query_row(params![digest.to_string()], |row| row.get::<_, bool>(0)) {
            Ok(pending) => Ok(pending),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Records what the artifact's own header says about it.
    ///
    /// # Errors
    /// Registry failure.
    pub fn record_semantics(
        &self,
        digest: &Digest,
        semantics: &Semantics,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE artifacts SET kind = ?2, name = ?3, architecture = ?4, quant = ?5, params = ?6
             WHERE digest = ?1",
            params![
                digest.to_string(),
                semantics.kind,
                semantics.name,
                semantics.architecture,
                semantics.quant,
                semantics.params
            ],
        )?;
        Ok(())
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
        stamp: FileStamp,
    ) -> Result<Option<Digest>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT digest FROM digest_cache
             WHERE path = ?1 AND device = ?2 AND inode = ?3 AND size = ?4
               AND mtime_secs = ?5 AND mtime_nanos = ?6
               AND ctime_secs = ?7 AND ctime_nanos = ?8",
        )?;
        let digest = statement
            .query_row(
                params![
                    path.to_string_lossy(),
                    stamp.device,
                    stamp.inode,
                    stamp.size,
                    stamp.mtime_secs,
                    stamp.mtime_nanos,
                    stamp.ctime_secs,
                    stamp.ctime_nanos
                ],
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

    /// Remembers a computed digest for a fully stamped file identity.
    ///
    /// # Errors
    /// Registry failure.
    pub fn remember_digest(
        &self,
        path: &Path,
        stamp: FileStamp,
        digest: &Digest,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO digest_cache
                 (path, device, inode, size, mtime_secs, mtime_nanos, ctime_secs, ctime_nanos, digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(path) DO UPDATE SET
                 device = ?2, inode = ?3, size = ?4,
                 mtime_secs = ?5, mtime_nanos = ?6,
                 ctime_secs = ?7, ctime_nanos = ?8, digest = ?9",
            params![
                path.to_string_lossy(),
                stamp.device,
                stamp.inode,
                stamp.size,
                stamp.mtime_secs,
                stamp.mtime_nanos,
                stamp.ctime_secs,
                stamp.ctime_nanos,
                digest.to_string()
            ],
        )?;
        Ok(())
    }

    /// Returns whether `path` is still the exact file modeld recorded as sharing
    /// the canonical blob for `digest`.
    ///
    /// # Errors
    /// Registry or stat failure.
    pub fn path_is_shared(&self, digest: &Digest, path: &Path) -> Result<bool, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT device, inode, size, mtime_secs, mtime_nanos, ctime_secs, ctime_nanos
             FROM shared_paths WHERE path = ?1 AND digest = ?2",
        )?;
        let stored =
            statement.query_row(params![path.to_string_lossy(), digest.to_string()], |row| {
                Ok(FileStamp {
                    device: row.get(0)?,
                    inode: row.get(1)?,
                    size: row.get(2)?,
                    mtime_secs: row.get(3)?,
                    mtime_nanos: row.get(4)?,
                    ctime_secs: row.get(5)?,
                    ctime_nanos: row.get(6)?,
                })
            });
        match stored {
            Ok(stored) => Ok(FileStamp::of(path)? == stored),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Records the current file at `path` as an APFS clone of `digest`.
    ///
    /// # Errors
    /// Registry or stat failure.
    pub fn record_shared_path(&self, digest: &Digest, path: &Path) -> Result<(), StoreError> {
        self.record_shared_stamp(digest, path, FileStamp::of(path)?)
    }

    /// Forgets clone-state metadata for a path restored to an independent copy.
    ///
    /// # Errors
    /// Registry failure.
    pub fn forget_shared_path(&self, path: &Path) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM shared_paths WHERE path = ?1",
            params![path.to_string_lossy()],
        )?;
        Ok(())
    }

    fn record_shared_stamp(
        &self,
        digest: &Digest,
        path: &Path,
        stamp: FileStamp,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO shared_paths
                 (path, digest, device, inode, size, mtime_secs, mtime_nanos, ctime_secs, ctime_nanos)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(path) DO UPDATE SET
                 digest = ?2, device = ?3, inode = ?4, size = ?5,
                 mtime_secs = ?6, mtime_nanos = ?7,
                 ctime_secs = ?8, ctime_nanos = ?9",
            params![
                path.to_string_lossy(),
                digest.to_string(),
                stamp.device,
                stamp.inode,
                stamp.size,
                stamp.mtime_secs,
                stamp.mtime_nanos,
                stamp.ctime_secs,
                stamp.ctime_nanos
            ],
        )?;
        Ok(())
    }

    /// All stored artifacts with their references, largest first (for `ls`).
    ///
    /// # Errors
    /// Registry failure, or a corrupt digest in the registry.
    pub fn artifacts(&self) -> Result<Vec<StoredArtifact>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT digest, size, format, kind, name, architecture, quant, params
             FROM artifacts ORDER BY size DESC",
        )?;
        let rows = statement
            .query_map([], |row| {
                let semantics = row
                    .get::<_, Option<String>>(3)?
                    .map(|kind| -> Result<Semantics, rusqlite::Error> {
                        Ok(Semantics {
                            kind,
                            name: row.get(4)?,
                            architecture: row.get(5)?,
                            quant: row.get(6)?,
                            params: row.get(7)?,
                        })
                    })
                    .transpose()?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    semantics,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut artifacts = Vec::with_capacity(rows.len());
        for (digest_text, size, format, semantics) in rows {
            let Ok(digest) = digest_text.parse::<Digest>() else {
                continue;
            };
            let references = self.references_for(&digest_text)?;
            artifacts.push(StoredArtifact {
                digest,
                size,
                format,
                references,
                semantics,
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

/// Monotonic-enough wall-clock stamp for distinguishing adjacent sync runs.
///
/// Nanoseconds avoid retaining stale rows when two syncs start in the same second.
#[must_use]
pub fn sync_stamp() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

fn migrate_semantics_columns(conn: &Connection) -> Result<(), rusqlite::Error> {
    let mut statement = conn.prepare("PRAGMA table_info(artifacts)")?;
    let existing = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    for column in ["kind", "name", "architecture", "quant", "params"] {
        if !existing.contains(column) {
            conn.execute_batch(&format!("ALTER TABLE artifacts ADD COLUMN {column} TEXT;"))?;
        }
    }
    Ok(())
}

fn migrate_stamp_columns(conn: &Connection, table: &str) -> Result<(), rusqlite::Error> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let existing = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<HashSet<_>, _>>()?;
    for column in ["device", "inode", "ctime_secs", "ctime_nanos"] {
        if !existing.contains(column) {
            conn.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} INTEGER NOT NULL DEFAULT 0;"
            ))?;
        }
    }
    Ok(())
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
            .import_blob(&source, &digest, Some(&Format::Gguf))
            .expect("import");
        let second = store
            .import_blob(&source, &digest, Some(&Format::Gguf))
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
            .import_blob(&source, &digest, Some(&Format::Gguf))
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
        store.import_blob(&source, &digest, None).expect("import");
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
        let stamp = FileStamp {
            device: 1,
            inode: 2,
            size: 10,
            mtime_secs: 1000,
            mtime_nanos: 500,
            ctime_secs: 1001,
            ctime_nanos: 600,
        };
        store
            .remember_digest(&path, stamp, &digest)
            .expect("remember");

        let hit = store.cached_digest(&path, stamp).expect("query");
        let stale = store
            .cached_digest(
                &path,
                FileStamp {
                    ctime_nanos: 601,
                    ..stamp
                },
            )
            .expect("query");

        assert_eq!(hit, Some(digest));
        assert_eq!(stale, None);
    }

    #[test]
    fn find_matches_labels_case_insensitively() {
        let (dir, store) = store();
        let (source, digest) = digest_of(b"weights", dir.path());
        store.import_blob(&source, &digest, None).expect("import");
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

    #[test]
    fn imported_source_is_remembered_only_while_unchanged() {
        let (dir, store) = store();
        let (source, digest) = digest_of(b"weights", dir.path());
        store.import_blob(&source, &digest, None).expect("import");

        assert!(store.path_is_shared(&digest, &source).expect("shared"));
        std::fs::write(&source, b"changed").expect("mutate source");
        assert!(!store.path_is_shared(&digest, &source).expect("stale"));
    }

    #[test]
    fn semantics_recorded_once_then_no_longer_pending() {
        let (dir, store) = store();
        let (source, digest) = digest_of(b"weights", dir.path());
        store
            .import_blob(&source, &digest, Some(&Format::Gguf))
            .expect("import");

        assert!(store.semantics_pending(&digest).expect("pending"));
        let semantics = Semantics {
            kind: "model".to_string(),
            name: Some("Test Model".to_string()),
            architecture: Some("llama".to_string()),
            quant: Some("Q4_K_M".to_string()),
            params: Some("1.7B".to_string()),
        };
        store.record_semantics(&digest, &semantics).expect("record");

        assert!(!store.semantics_pending(&digest).expect("pending"));
        let artifacts = store.artifacts().expect("artifacts");
        assert_eq!(artifacts[0].semantics.as_ref(), Some(&semantics));
    }

    #[test]
    fn semantics_not_pending_for_unknown_digest() {
        let (_dir, store) = store();
        let digest = Digest::new(Algorithm::Sha256, vec![7; 32]).expect("digest");
        assert!(!store.semantics_pending(&digest).expect("pending"));
    }

    #[test]
    fn repairs_a_corrupt_existing_canonical() {
        let (dir, store) = store();
        let (source, digest) = digest_of(b"weights", dir.path());
        let blob = store.blob_path(&digest);
        std::fs::write(&blob, b"corrupt").expect("seed corrupt blob");

        let outcome = store.import_blob(&source, &digest, None).expect("repair");

        assert_eq!(outcome, ImportOutcome::Imported);
        assert_eq!(std::fs::read(blob).expect("read repaired"), b"weights");
    }
}
