//! One sync pass: verify digests, import blobs, record references, prune stale ones.
//!
//! Shared by `modeld sync` and every daemon pass. The pass never touches provider
//! files — it only clones them into the store and updates the registry.
//!
//! A non-zero settle window defers files written within it: a file that fresh
//! may still be downloading, and importing a half-written file would register a
//! junk artifact. Deferred files keep their existing references (pruning is
//! skipped for an incomplete pass) and are picked up once they settle.

use crate::semantics;
use modeld_core::{Algorithm, Artifact, Digest};
use modeld_providers::scan::{ScanOutcome, Skipped};
use modeld_store::{FileStamp, ImportOutcome, Store};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// What one sync pass did.
#[derive(Debug, Default)]
pub struct SyncReport {
    /// Blobs newly cloned into the store.
    pub imported: usize,
    /// Artifacts with verified sha256 digests whose references were recorded.
    pub synced: Vec<Artifact>,
    /// Stale references dropped; `None` when the pass was incomplete.
    pub pruned: Option<usize>,
    /// Files this pass (or the scan feeding it) could not sync.
    pub skipped: Vec<Skipped>,
    /// Files written too recently to trust yet.
    pub deferred: Vec<Deferred>,
    /// Non-fatal problems worth surfacing.
    pub warnings: Vec<String>,
}

impl SyncReport {
    /// Whether every trackable scanned file was synced, making stale
    /// references provable.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.skipped.iter().all(|skip| skip.untrackable) && self.deferred.is_empty()
    }

    /// Time until the soonest deferred file settles, if any file was deferred.
    #[must_use]
    pub fn next_settle(&self) -> Option<Duration> {
        self.deferred
            .iter()
            .map(|deferred| deferred.settles_in)
            .min()
    }
}

/// A file skipped because it was written within the settle window.
#[derive(Debug, Clone)]
pub struct Deferred {
    pub path: PathBuf,
    pub settles_in: Duration,
}

/// Outcome of syncing one artifact.
enum Step {
    Synced(ImportOutcome),
    Deferred(Duration),
    Skipped(String),
    /// On another volume: the store can never clone it, so it holds no reference.
    NotCloneable,
    /// Not a sha256 artifact; the store cannot hold it, which is not a failure.
    Ignored,
}

/// Syncs every scanned artifact into `store`.
///
/// `settle` defers files modified within that window (`Duration::ZERO` syncs
/// everything). `progress` is called with each path before it is hashed.
pub fn run(
    store: &Store,
    outcome: ScanOutcome,
    settle: Duration,
    mut progress: impl FnMut(&Path, u64),
) -> SyncReport {
    let stamp = modeld_store::sync_stamp();
    let now = SystemTime::now();
    let mut report = SyncReport {
        skipped: outcome.skipped,
        ..SyncReport::default()
    };

    for mut artifact in outcome.artifacts {
        match sync_artifact(store, &mut artifact, settle, now, stamp, &mut progress) {
            Step::Synced(imported) => {
                if imported == ImportOutcome::Imported {
                    report.imported += 1;
                }
                record_semantics_if_pending(store, &artifact, &mut report.warnings);
                report.synced.push(artifact);
            }
            Step::Deferred(settles_in) => {
                report.deferred.push(Deferred {
                    path: artifact.path,
                    settles_in,
                });
            }
            Step::Skipped(reason) => {
                report.skipped.push(Skipped {
                    path: artifact.path,
                    reason,
                    untrackable: false,
                });
            }
            Step::NotCloneable => {
                report.skipped.push(Skipped {
                    path: artifact.path,
                    reason: "different volume".to_string(),
                    untrackable: true,
                });
            }
            Step::Ignored => {}
        }
    }

    if report.is_complete() {
        report.pruned = prune(store, stamp, &mut report.warnings);
    }
    report
}

fn sync_artifact(
    store: &Store,
    artifact: &mut Artifact,
    settle: Duration,
    now: SystemTime,
    stamp: u64,
    progress: &mut impl FnMut(&Path, u64),
) -> Step {
    if let Some(settles_in) = unsettled(&artifact.path, settle, now) {
        return Step::Deferred(settles_in);
    }
    let Some(verified) = ensure_verified_digest(store, artifact, progress) else {
        return Step::Skipped("file changed or could not be hashed".to_string());
    };
    let Some(digest) = artifact.digest.clone() else {
        return Step::Ignored;
    };
    if digest.algorithm() != Algorithm::Sha256 {
        return Step::Ignored;
    }
    let format = artifact.format.as_ref();
    let imported = match store.import_blob(&artifact.path, verified, &digest, format) {
        Ok(ImportOutcome::NotCloneable) => return Step::NotCloneable,
        Ok(imported) => imported,
        Err(error) => return Step::Skipped(error.to_string()),
    };
    // A rewrite after hashing must not be recorded under the old digest.
    if !FileStamp::of(&artifact.path).is_ok_and(|now| now == verified) {
        return Step::Skipped("file changed while importing".to_string());
    }
    let recorded = store.record_reference(
        &digest,
        &artifact.path,
        artifact.provider,
        artifact.label.as_deref(),
        stamp,
    );
    match recorded {
        Ok(()) => Step::Synced(imported),
        Err(error) => Step::Skipped(format!("reference not recorded: {error}")),
    }
}

fn prune(store: &Store, stamp: u64, warnings: &mut Vec<String>) -> Option<usize> {
    match store.prune_references_before(stamp) {
        Ok(pruned) => Some(pruned),
        Err(error) => {
            warnings.push(format!("stale references were not pruned ({error})"));
            None
        }
    }
}

/// Time left until a file at `path` settles; `None` once settled or unstat-able.
///
/// A file that cannot be stat-ed is left to digest verification, which skips
/// it with a clearer reason.
fn unsettled(path: &Path, settle: Duration, now: SystemTime) -> Option<Duration> {
    if settle.is_zero() {
        return None;
    }
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    remaining_settle(modified, now, settle)
}

/// Time left before a file modified at `modified` has been quiet for `window`.
///
/// A modification time in the future (clock skew) counts as "just written".
fn remaining_settle(modified: SystemTime, now: SystemTime, window: Duration) -> Option<Duration> {
    let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
    window
        .checked_sub(age)
        .filter(|remaining| !remaining.is_zero())
}

/// Reads header facts for a newly stored artifact; later syncs skip it.
fn record_semantics_if_pending(store: &Store, artifact: &Artifact, warnings: &mut Vec<String>) {
    let Some(digest) = artifact.digest.as_ref() else {
        return;
    };
    match store.semantics_pending(digest) {
        Ok(true) => {
            let blob = store.blob_path(digest);
            let semantics = semantics::analyze(artifact, &blob, |warning| warnings.push(warning));
            if let Err(error) = store.record_semantics(digest, &semantics) {
                warnings.push(format!(
                    "could not record semantics for {} ({error})",
                    artifact.path.display()
                ));
            }
        }
        Ok(false) => {}
        Err(error) => warnings.push(format!(
            "semantics check failed for {} ({error})",
            artifact.path.display()
        )),
    }
}

/// Verifies (or computes) the artifact's digest, consulting the store's cache.
///
/// Harvested digests are claims; sync trusts only hashes modeld computed itself.
/// Returns the stamp the digest was verified at, or `None` when the file cannot
/// be hashed or changed while hashing.
fn ensure_verified_digest(
    store: &Store,
    artifact: &mut Artifact,
    progress: &mut impl FnMut(&Path, u64),
) -> Option<FileStamp> {
    let before = FileStamp::of(&artifact.path).ok()?;
    if artifact.digest_verified {
        return Some(before);
    }
    if before.size != artifact.size {
        return None;
    }
    if let Ok(Some(cached)) = store.cached_digest(&artifact.path, before)
        && FileStamp::of(&artifact.path).is_ok_and(|after| after == before)
    {
        artifact.digest = Some(cached);
        artifact.digest_verified = true;
        return Some(before);
    }
    progress(&artifact.path, artifact.size);
    let actual = Digest::sha256_file(&artifact.path).ok()?;
    if !FileStamp::of(&artifact.path).is_ok_and(|after| after == before) {
        return None;
    }
    let _ = store.remember_digest(&artifact.path, before, &actual);
    artifact.digest = Some(actual);
    artifact.digest_verified = true;
    Some(before)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use modeld_core::ProviderKind;
    use modeld_providers::ProviderRoot;

    /// A temp home with a Manual model root and a store, as the daemon sees it.
    pub(crate) struct Fixture {
        pub dir: tempfile::TempDir,
        pub root: ProviderRoot,
        pub store: Store,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            let dir = tempfile::tempdir().expect("create temp dir");
            let models = dir.path().join("models");
            std::fs::create_dir_all(&models).expect("create model root");
            let store = Store::open(dir.path().join(".modeld")).expect("open store");
            let root = ProviderRoot {
                kind: ProviderKind::Manual,
                root: models,
                excluded: vec![],
                label_prefix: None,
            };
            Self { dir, root, store }
        }

        /// Writes a model file backdated past any settle window.
        pub(crate) fn settled_model(&self, name: &str, content: &[u8]) -> PathBuf {
            let path = self.fresh_model(name, content);
            let old = SystemTime::now() - Duration::from_hours(1);
            std::fs::File::open(&path)
                .expect("open model")
                .set_times(std::fs::FileTimes::new().set_modified(old))
                .expect("backdate model");
            path
        }

        pub(crate) fn fresh_model(&self, name: &str, content: &[u8]) -> PathBuf {
            let path = self.root.root.join(name);
            std::fs::write(&path, content).expect("write model");
            path
        }

        pub(crate) fn scan(&self) -> ScanOutcome {
            modeld_providers::scan::scan(std::slice::from_ref(&self.root), 1)
        }

        pub(crate) fn sync(&self, settle: Duration) -> SyncReport {
            run(&self.store, self.scan(), settle, |_, _| {})
        }
    }

    #[test]
    fn settled_file_is_imported_and_referenced() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");

        let report = fixture.sync(Duration::from_mins(5));

        assert_eq!(report.imported, 1);
        assert_eq!(report.synced.len(), 1);
        assert_eq!(report.pruned, Some(0));
        assert_eq!(
            fixture.store.artifacts().expect("artifacts")[0]
                .references
                .len(),
            1
        );
    }

    #[test]
    fn fresh_file_is_deferred_and_blocks_pruning() {
        let fixture = Fixture::new();
        fixture.fresh_model("downloading.gguf", b"weights");

        let report = fixture.sync(Duration::from_mins(5));

        assert_eq!(report.imported, 0);
        assert!(report.synced.is_empty());
        assert_eq!(report.pruned, None);
        let settles_in = report.next_settle().expect("deferred");
        assert!(settles_in > Duration::from_mins(4) && settles_in <= Duration::from_mins(5));
    }

    #[test]
    fn zero_settle_window_syncs_fresh_files() {
        let fixture = Fixture::new();
        fixture.fresh_model("a.gguf", b"weights");

        let report = fixture.sync(Duration::ZERO);

        assert_eq!(report.imported, 1);
        assert!(report.deferred.is_empty());
    }

    #[test]
    fn empty_complete_scan_prunes_references_to_vanished_files() {
        let fixture = Fixture::new();
        let model = fixture.settled_model("a.gguf", b"weights");
        fixture.sync(Duration::ZERO);
        std::fs::remove_file(&model).expect("delete model");

        let report = run(
            &fixture.store,
            ScanOutcome::new(),
            Duration::ZERO,
            |_, _| {},
        );

        assert_eq!(report.pruned, Some(1));
    }

    /// A scan of nothing but one skip, after `a.gguf` was synced and deleted.
    fn run_with_one_skip(untrackable: bool) -> SyncReport {
        let fixture = Fixture::new();
        let model = fixture.settled_model("a.gguf", b"weights");
        fixture.sync(Duration::ZERO);
        std::fs::remove_file(&model).expect("delete model");
        let mut scanned = ScanOutcome::new();
        scanned.skipped.push(Skipped {
            path: fixture.root.root.join("sha256-partial"),
            reason: "test skip".to_string(),
            untrackable,
        });

        run(&fixture.store, scanned, Duration::ZERO, |_, _| {})
    }

    #[test]
    fn untrackable_skip_does_not_block_pruning() {
        assert_eq!(run_with_one_skip(true).pruned, Some(1));
    }

    #[test]
    fn failed_skip_blocks_pruning() {
        assert_eq!(run_with_one_skip(false).pruned, None);
    }

    #[test]
    fn remaining_settle_counts_down_from_the_last_write() {
        let now = SystemTime::now();
        let window = Duration::from_mins(5);

        assert_eq!(
            remaining_settle(now - Duration::from_mins(1), now, window),
            Some(Duration::from_mins(4))
        );
        assert_eq!(
            remaining_settle(now - Duration::from_mins(5), now, window),
            None
        );
        assert_eq!(
            remaining_settle(now + Duration::from_mins(1), now, window),
            Some(window)
        );
    }
}
