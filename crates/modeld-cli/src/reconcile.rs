//! One daemon pass: sync every root, then clone duplicates of stored blobs.
//!
//! The pass holds the [`StoreLock`] throughout, so it never interleaves with
//! a manual `sync`, `dedupe`, `restore`, or `gc`; waiting for that lock ends
//! early when the daemon is asked to stop. Files still settling are
//! deferred by the sync step and never reach consolidation; the report says
//! when the soonest of them settles so the daemon can come back for it.

use crate::sync::{self, SyncReport};
use modeld_core::consolidate::{Journal, Replacement, Report};
use modeld_providers::Detection;
use modeld_store::{Store, StoreError, StoreLock};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often a pass waiting for the store lock re-checks the stop flag.
const LOCK_POLL: Duration = Duration::from_millis(250);

/// How a pass behaves.
#[derive(Debug, Clone, Copy)]
pub struct PassOptions {
    /// Defer files modified within this window.
    pub settle: Duration,
    /// Ignore files smaller than this many bytes.
    pub min_size: u64,
    /// Sync the registry but only report the clones a real pass would make.
    pub dry_run: bool,
}

/// What one pass did.
#[derive(Debug)]
pub struct PassReport {
    pub sync: SyncReport,
    /// Replacements planned against the store.
    pub planned: Vec<Replacement>,
    /// What consolidation did; `None` on a dry run or when nothing was planned.
    pub consolidation: Option<Report>,
    /// Non-fatal bookkeeping problems from consolidation.
    pub warnings: Vec<String>,
}

/// Runs one pass over the detected roots against the store at `store_root`.
///
/// Returns `None`, having changed nothing, when `stop` was raised before the
/// store lock came free. `progress` is called with each path before it is
/// hashed.
///
/// # Errors
/// Locking or opening the store or its journal failed; nothing was changed.
pub fn run(
    store_root: &Path,
    detection: &Detection,
    options: PassOptions,
    stop: &AtomicBool,
    progress: impl FnMut(&Path, u64),
) -> Result<Option<PassReport>, StoreError> {
    let Some(_lock) =
        StoreLock::acquire_unless(store_root, LOCK_POLL, || stop.load(Ordering::Relaxed))?
    else {
        return Ok(None);
    };
    let store = Store::open(store_root.to_path_buf())?;
    let journal = Journal::open(store_root.join("journal.jsonl"))?;

    let scanned = detection.scan(options.min_size);
    let sync = sync::run(&store, scanned, options.settle, progress);
    let planned = crate::consolidation::plan_against_store(&store, &sync.synced);
    let mut warnings = Vec::new();
    let consolidation = (!options.dry_run && !planned.is_empty()).then(|| {
        crate::consolidation::apply(&store, &journal, &planned, |warning| warnings.push(warning))
    });
    Ok(Some(PassReport {
        sync,
        planned,
        consolidation,
        warnings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::tests::Fixture;
    use modeld_providers::scan::Skipped;

    const OPTIONS: PassOptions = PassOptions {
        settle: Duration::from_mins(5),
        min_size: 1,
        dry_run: false,
    };

    /// A stop flag that is never raised.
    static RUNNING: AtomicBool = AtomicBool::new(false);

    fn pass(fixture: &Fixture, options: PassOptions) -> PassReport {
        let detection = Detection {
            roots: vec![fixture.root.clone()],
            unreadable: vec![],
        };
        run(
            &fixture.dir.path().join(".modeld"),
            &detection,
            options,
            &RUNNING,
            |_, _| {},
        )
        .expect("pass")
        .expect("not stopped")
    }

    fn journal_len(fixture: &Fixture) -> usize {
        Journal::open(fixture.dir.path().join(".modeld/journal.jsonl"))
            .and_then(|journal| journal.entries())
            .expect("journal")
            .len()
    }

    #[test]
    fn pass_clones_a_settled_duplicate_and_the_next_pass_is_a_no_op() {
        let fixture = Fixture::new();
        fixture.settled_model("lmstudio-copy.gguf", b"weights");
        fixture.settled_model("hf-copy.gguf", b"weights");

        let first = pass(&fixture, OPTIONS);
        let second = pass(&fixture, OPTIONS);

        assert_eq!(first.sync.imported, 1);
        assert_eq!(first.consolidation.expect("real pass").completed.len(), 1);
        assert!(second.planned.is_empty());
        assert_eq!(journal_len(&fixture), 1);
    }

    #[test]
    fn dry_run_plans_without_touching_provider_files() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");
        fixture.settled_model("b.gguf", b"weights");

        let report = pass(
            &fixture,
            PassOptions {
                dry_run: true,
                ..OPTIONS
            },
        );

        assert_eq!(report.planned.len(), 1);
        assert!(report.consolidation.is_none());
        assert_eq!(journal_len(&fixture), 0);
    }

    #[test]
    fn settling_download_is_left_for_a_later_pass() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");
        fixture.fresh_model("still-downloading.gguf", b"weights");

        let report = pass(&fixture, OPTIONS);

        assert!(report.planned.is_empty());
        assert!(report.sync.next_settle().is_some());
    }

    #[test]
    fn unreadable_config_keeps_references_its_roots_owned() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");
        pass(&fixture, OPTIONS);
        let broken = Detection {
            roots: vec![],
            unreadable: vec![Skipped {
                path: fixture.dir.path().join(".modeld/config.toml"),
                reason: "invalid TOML".to_string(),
                untrackable: false,
            }],
        };

        let report = run(
            &fixture.dir.path().join(".modeld"),
            &broken,
            OPTIONS,
            &RUNNING,
            |_, _| {},
        )
        .expect("pass")
        .expect("not stopped");

        assert_eq!(report.sync.pruned, None);
        let artifacts = fixture.store.artifacts().expect("artifacts");
        assert_eq!(artifacts[0].references.len(), 1);
    }

    #[test]
    fn pass_waits_for_a_held_store_lock() {
        let fixture = Fixture::new();
        let store_root = fixture.dir.path().join(".modeld");
        let held = StoreLock::acquire(&store_root).expect("hold lock");
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        let pass_root = store_root.clone();
        let pass = std::thread::spawn(move || {
            let report = run(
                &pass_root,
                &Detection::default(),
                OPTIONS,
                &RUNNING,
                |_, _| {},
            );
            done_tx.send(matches!(report, Ok(Some(_)))).expect("send");
        });

        let early = done_rx.recv_timeout(Duration::from_millis(200));
        drop(held);
        pass.join().expect("pass thread");

        assert!(early.is_err(), "pass ran while the lock was held");
        assert_eq!(done_rx.recv(), Ok(true));
    }

    #[test]
    fn stop_ends_the_wait_for_a_held_store_lock() {
        let fixture = Fixture::new();
        let store_root = fixture.dir.path().join(".modeld");
        let _held = StoreLock::acquire(&store_root).expect("hold lock");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        let pass_stop = std::sync::Arc::clone(&stop);
        let pass = std::thread::spawn(move || {
            let report = run(
                &store_root,
                &Detection::default(),
                OPTIONS,
                &pass_stop,
                |_, _| {},
            );
            done_tx.send(matches!(report, Ok(None))).expect("send");
        });
        stop.store(true, Ordering::Relaxed);

        assert_eq!(done_rx.recv_timeout(Duration::from_secs(5)), Ok(true));
        pass.join().expect("pass thread");
    }
}
