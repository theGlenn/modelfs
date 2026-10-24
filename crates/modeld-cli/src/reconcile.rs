//! One daemon pass: sync every root, then clone duplicates of stored blobs.
//!
//! The pass holds the [`StoreLock`] throughout, so it never interleaves with
//! a manual `sync`, `dedupe`, `restore`, or `gc`; waiting for that lock ends
//! early when the daemon is asked to stop. Files still settling are
//! deferred by the sync step and never reach consolidation; the report says
//! when the soonest of them settles so the daemon can come back for it. A
//! finished pass moves the store's [generation](crate::generation) forward.

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

/// Progress of one pass, reported while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassEvent<'a> {
    /// Another modeld process holds the store lock; the pass waits for it.
    WaitingForLock,
    /// The pass holds the store lock; from here on, a stop request no
    /// longer ends it early.
    Locked,
    /// A file is about to be hashed.
    Hashing { path: &'a Path, size: u64 },
    /// A duplicate is about to be replaced with a clone of its blob.
    Cloning { path: &'a Path, size: u64 },
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
/// store lock came free. `on_event` hears the pass's progress.
///
/// # Errors
/// Locking or opening the store or its journal failed; nothing was changed.
pub fn run(
    store_root: &Path,
    detection: &Detection,
    options: PassOptions,
    stop: &AtomicBool,
    mut on_event: impl FnMut(PassEvent<'_>),
) -> Result<Option<PassReport>, StoreError> {
    let Some(_lock) = lock(store_root, stop, &mut on_event)? else {
        return Ok(None);
    };
    on_event(PassEvent::Locked);
    let store = Store::open(store_root.to_path_buf())?;
    let journal = Journal::open(store_root.join("journal.jsonl"))?;

    let scanned = detection.scan(options.min_size);
    let sync = sync::run(&store, scanned, options.settle, |path, size| {
        on_event(PassEvent::Hashing { path, size });
    });
    let planned = crate::consolidation::plan_against_store(&store, &sync.synced);
    let mut warnings = Vec::new();
    let consolidation = (!options.dry_run && !planned.is_empty()).then(|| {
        crate::consolidation::apply(
            &store,
            &journal,
            &planned,
            |replacement| {
                on_event(PassEvent::Cloning {
                    path: &replacement.victim,
                    size: replacement.size,
                });
            },
            |warning| warnings.push(warning),
        )
    });
    if let Err(error) = crate::generation::bump(store_root) {
        warnings.push(format!("could not update the store generation ({error})"));
    }
    Ok(Some(PassReport {
        sync,
        planned,
        consolidation,
        warnings,
    }))
}

/// Takes the store lock, waiting for it unless `stop` is raised first.
///
/// `on_event` hears [`PassEvent::WaitingForLock`] once, when another holder
/// makes the pass wait.
fn lock(
    store_root: &Path,
    stop: &AtomicBool,
    on_event: &mut impl FnMut(PassEvent<'_>),
) -> Result<Option<StoreLock>, StoreError> {
    let stopped = || stop.load(Ordering::Relaxed);
    if stopped() {
        return Ok(None);
    }
    if let Some(lock) = StoreLock::try_acquire(store_root)? {
        return Ok(Some(lock));
    }
    on_event(PassEvent::WaitingForLock);
    StoreLock::acquire_unless(store_root, LOCK_POLL, stopped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::tests::Fixture;
    use modeld_providers::scan::Skipped;
    use std::path::PathBuf;

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
            |_| {},
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
            |_| {},
        )
        .expect("pass")
        .expect("not stopped");

        assert_eq!(report.sync.pruned, None);
        let artifacts = fixture.store.artifacts().expect("artifacts");
        assert_eq!(artifacts[0].references.len(), 1);
    }

    /// The events of one pass, owned so they outlive it.
    #[derive(Debug, PartialEq, Eq)]
    enum Heard {
        WaitingForLock,
        Locked,
        Hashing(PathBuf),
        Cloning(PathBuf),
    }

    impl From<PassEvent<'_>> for Heard {
        fn from(event: PassEvent<'_>) -> Self {
            match event {
                PassEvent::WaitingForLock => Self::WaitingForLock,
                PassEvent::Locked => Self::Locked,
                PassEvent::Hashing { path, .. } => Self::Hashing(path.to_path_buf()),
                PassEvent::Cloning { path, .. } => Self::Cloning(path.to_path_buf()),
            }
        }
    }

    #[test]
    fn pass_reports_locking_hashing_and_cloning_in_order() {
        let fixture = Fixture::new();
        let first = fixture.settled_model("a.gguf", b"weights");
        let second = fixture.settled_model("b.gguf", b"weights");
        let detection = Detection {
            roots: vec![fixture.root.clone()],
            unreadable: vec![],
        };
        let mut heard = Vec::new();

        run(
            &fixture.dir.path().join(".modeld"),
            &detection,
            OPTIONS,
            &RUNNING,
            |event| heard.push(Heard::from(event)),
        )
        .expect("pass")
        .expect("not stopped");

        assert_eq!(heard[0], Heard::Locked);
        let hashed: Vec<&Heard> = heard
            .iter()
            .filter(|event| matches!(event, Heard::Hashing(_)))
            .collect();
        assert_eq!(hashed.len(), 2, "{heard:?}");
        let cloned = heard.last().expect("events");
        assert!(
            *cloned == Heard::Cloning(first) || *cloned == Heard::Cloning(second),
            "{heard:?}"
        );
    }

    #[test]
    fn finished_pass_moves_the_store_generation_forward() {
        let fixture = Fixture::new();
        let generation = fixture.dir.path().join(".modeld/generation");

        pass(&fixture, OPTIONS);
        let first = std::fs::read_to_string(&generation).expect("first generation");
        pass(&fixture, OPTIONS);

        assert_ne!(
            std::fs::read_to_string(&generation).expect("second generation"),
            first
        );
    }

    #[test]
    fn pass_waits_for_a_held_store_lock() {
        let fixture = Fixture::new();
        let store_root = fixture.dir.path().join(".modeld");
        let held = StoreLock::acquire(&store_root).expect("hold lock");
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();

        let pass_root = store_root.clone();
        let pass = std::thread::spawn(move || {
            let report = run(
                &pass_root,
                &Detection::default(),
                OPTIONS,
                &RUNNING,
                |event| {
                    if event == PassEvent::WaitingForLock {
                        waiting_tx.send(()).expect("send");
                    }
                },
            );
            done_tx.send(matches!(report, Ok(Some(_)))).expect("send");
        });

        let waited = waiting_rx.recv_timeout(Duration::from_secs(5));
        let early = done_rx.recv_timeout(Duration::from_millis(200));
        drop(held);
        pass.join().expect("pass thread");

        assert_eq!(waited, Ok(()), "pass never said it was waiting");
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
                |_| {},
            );
            done_tx.send(matches!(report, Ok(None))).expect("send");
        });
        stop.store(true, Ordering::Relaxed);

        assert_eq!(done_rx.recv_timeout(Duration::from_secs(5)), Ok(true));
        pass.join().expect("pass thread");
    }
}
