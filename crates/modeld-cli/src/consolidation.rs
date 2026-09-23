//! Store-anchored consolidation: make synced duplicates clones of their blob.
//!
//! After a sync every artifact has a verified digest and a canonical blob. Any
//! synced file that is not yet known to share the blob's extents holds its own
//! copy of bytes the store already has — a duplicate, even when it is the only
//! provider copy (say, a model re-downloaded after the original was deleted).
//! Unlike `dedupe`'s group planning, this catches that single-copy case.
//!
//! "Known to share" means recorded in the registry, or — for clones made
//! before modeld recorded them — confirmed by APFS block mapping.

use crate::plan;
use modeld_core::consolidate::{Journal, Replacement, Report};
use modeld_core::{Artifact, Digest, FileId};
use modeld_store::{FileStamp, Store};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Replacements turning each synced duplicate into a clone of its store blob.
///
/// A registry error while checking a path counts as "already shared": when in
/// doubt, leave the file alone. Files found physically sharing the blob
/// without a registry record are adopted (recorded) instead of replaced.
pub fn plan_against_store(store: &Store, synced: &[Artifact]) -> Vec<Replacement> {
    synced
        .iter()
        .filter_map(|artifact| {
            let digest = artifact.digest.as_ref()?;
            let blob = store.blob_path(digest);
            let blob_id = file_id(&blob)?;
            let shared = store.path_is_shared(digest, &artifact.path).unwrap_or(true)
                || adopt_untracked_clone(store, digest, &artifact.path, &blob);
            plan::is_consolidatable(artifact, Some(blob_id), shared).then(|| Replacement {
                canonical: blob,
                victim: artifact.path.clone(),
                digest: digest.clone(),
                size: artifact.size,
            })
        })
        .collect()
}

/// Performs `replacements` and records each resulting clone in the registry.
///
/// Completed paths are remembered as sharing the blob (so they are never
/// planned again) and their digests cached (so the next sync does not re-hash
/// the fresh inode). Bookkeeping failures go to `warn`; the swap itself is
/// already journaled and restorable.
pub fn apply(
    store: &Store,
    journal: &Journal,
    replacements: &[Replacement],
    mut warn: impl FnMut(String),
) -> Report {
    let report = modeld_core::consolidate::consolidate(replacements, journal);
    for replacement in replacements {
        if !report.completed.contains(&replacement.victim) {
            continue;
        }
        let path = &replacement.victim;
        let recorded = FileStamp::of(path)
            .map_err(modeld_store::StoreError::from)
            .and_then(|stamp| {
                store.record_shared_path(&replacement.digest, path)?;
                store.remember_digest(path, stamp, &replacement.digest)
            });
        if let Err(error) = recorded {
            warn(format!(
                "could not remember clone state for {} ({error})",
                path.display()
            ));
        }
    }
    report
}

/// Records `path` as a clone of `blob` if APFS says they already share blocks.
///
/// Covers files cloned before modeld tracked clones, or by another tool:
/// replacing them would re-hash gigabytes and journal a swap that frees
/// nothing. Returns whether the file shares the blob's extents.
fn adopt_untracked_clone(store: &Store, digest: &Digest, path: &Path, blob: &Path) -> bool {
    if !modeld_core::apfs::shares_extents(path, blob).unwrap_or(false) {
        return false;
    }
    // Best-effort bookkeeping: the physical fact holds even if recording fails.
    let _ = store.record_shared_path(digest, path);
    true
}

fn file_id(path: &Path) -> Option<FileId> {
    std::fs::metadata(path).ok().map(|metadata| FileId {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::tests::Fixture;
    use std::time::Duration;

    fn journal(fixture: &Fixture) -> Journal {
        Journal::open(fixture.dir.path().join(".modeld/journal.jsonl")).expect("open journal")
    }

    #[test]
    fn imported_source_needs_no_replacement() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");

        let synced = fixture.sync(Duration::ZERO).synced;

        assert!(plan_against_store(&fixture.store, &synced).is_empty());
    }

    #[test]
    fn copy_of_a_stored_artifact_becomes_a_clone_of_its_blob() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");
        fixture.sync(Duration::ZERO);
        let copy = fixture.settled_model("redownloaded.gguf", b"weights");

        let synced = fixture.sync(Duration::ZERO).synced;
        let plan = plan_against_store(&fixture.store, &synced);

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].victim, copy);
        let report = apply(&fixture.store, &journal(&fixture), &plan, |w| panic!("{w}"));
        assert_eq!(report.completed, vec![copy.clone()]);
        assert_eq!(journal(&fixture).entries().expect("entries").len(), 1);
        assert_eq!(std::fs::read(&copy).expect("read clone"), b"weights");
    }

    #[test]
    fn untracked_clone_of_the_blob_is_adopted_not_replaced() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");
        let digest = fixture.sync(Duration::ZERO).synced[0]
            .digest
            .clone()
            .expect("digest");
        // A clone modeld never recorded, like imports that predate clone tracking.
        let untracked = fixture.root.root.join("untracked.gguf");
        modeld_core::apfs::clone_file(&fixture.store.blob_path(&digest), &untracked)
            .expect("clone blob");

        let synced = fixture.sync(Duration::ZERO).synced;

        assert!(plan_against_store(&fixture.store, &synced).is_empty());
        assert!(
            fixture
                .store
                .path_is_shared(&digest, &untracked)
                .expect("shared")
        );
    }

    #[test]
    fn applied_clone_is_neither_replanned_nor_rehashed() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");
        fixture.sync(Duration::ZERO);
        let copy = fixture.settled_model("b.gguf", b"weights");
        let plan = plan_against_store(&fixture.store, &fixture.sync(Duration::ZERO).synced);
        apply(&fixture.store, &journal(&fixture), &plan, |w| panic!("{w}"));

        let mut hashed = Vec::new();
        let report = crate::sync::run(&fixture.store, fixture.scan(), Duration::ZERO, |path, _| {
            hashed.push(path.to_path_buf());
        });

        assert!(hashed.is_empty(), "re-hashed {hashed:?}");
        assert!(plan_against_store(&fixture.store, &report.synced).is_empty());
        assert!(
            fixture
                .store
                .path_is_shared(report.synced[0].digest.as_ref().expect("digest"), &copy)
                .expect("shared")
        );
    }
}
