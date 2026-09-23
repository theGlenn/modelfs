//! `modeld daemon`: watch scan roots and keep the store converged.
//!
//! `FSEvents` (via `notify`) report changes under every detected scan root and
//! the store directory (for `config.toml` edits). [`Schedule`] turns events
//! into passes. Each pass re-detects roots, so config edits and newly created
//! project folders are picked up, re-arms the watches, and runs
//! [`reconcile::run`]: new downloads are imported once settled, and any file
//! duplicating a stored blob becomes a clone of it.
//!
//! SIGINT/SIGTERM stop the daemon between passes. A pass in flight always
//! completes, so a consolidation swap is never cut off before it is journaled;
//! a pass still waiting for the store lock is abandoned instead. A second
//! signal exits immediately.

use crate::events::EventFilter;
use crate::reconcile::{self, PassOptions, PassReport};
use crate::report::human_bytes;
use crate::schedule::{Schedule, Timings};
use modeld_providers::{Detection, ProviderRoot};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

/// Pass timing: react 10 s after activity stops, at most 2 min into a steady
/// stream of events, and reconcile every 15 min regardless.
const TIMINGS: Timings = Timings {
    quiet: Duration::from_secs(10),
    max_delay: Duration::from_mins(2),
    periodic: Duration::from_mins(15),
};

/// Longest the loop blocks before re-checking the stop flag.
const TICK: Duration = Duration::from_secs(1);

/// Slack added to a settle retry so the file is surely past its window.
const SETTLE_MARGIN: Duration = Duration::from_secs(1);

/// Back-off after a pass that could not lock or open the store.
const RETRY_AFTER_ERROR: Duration = Duration::from_mins(1);

/// Runs the daemon in the foreground until SIGINT or SIGTERM.
///
/// # Errors
/// Signal handlers or the file watcher could not be set up, or the watcher
/// stopped delivering events.
pub fn run(store_root: &Path, options: PassOptions) -> Result<(), Box<dyn std::error::Error>> {
    let stop = stop_flag()?;
    std::fs::create_dir_all(store_root)?;
    let Some(_instance) = claim_instance(store_root, &stop)? else {
        log("daemon stopped");
        return Ok(());
    };
    let (events_tx, events) = mpsc::channel();
    let mut watches = Watches::new(notify::recommended_watcher(events_tx)?);
    watches.watch_store(store_root)?;
    let mut filter = EventFilter::new(&[], store_root);
    let mut schedule = Schedule::new(Instant::now(), TIMINGS);
    log(if options.dry_run {
        "daemon started (dry run: clones are logged, never made)"
    } else {
        "daemon started"
    });

    while !stop.load(Ordering::Relaxed) {
        if schedule.is_due(Instant::now()) {
            let detection = modeld_providers::detect_all();
            watches.follow(&detection.roots);
            filter = EventFilter::new(&detection.roots, store_root);
            let retry_in = run_pass(store_root, &detection, options, &stop);
            schedule.pass_finished(Instant::now(), retry_in);
            continue;
        }
        match events.recv_timeout(schedule.time_until_due(Instant::now()).min(TICK)) {
            Ok(Ok(event)) if filter.wakes_daemon(&event) => schedule.record_event(Instant::now()),
            Ok(Ok(_)) | Err(RecvTimeoutError::Timeout) => {}
            Ok(Err(error)) => {
                log(format!("watcher error: {error}; rescanning"));
                schedule.record_event(Instant::now());
            }
            Err(RecvTimeoutError::Disconnected) => return Err("file watcher stopped".into()),
        }
    }
    log("daemon stopped");
    Ok(())
}

/// First SIGINT/SIGTERM raises the flag; a second one exits at once.
fn stop_flag() -> std::io::Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        // Order matters: the conditional exit must see the flag before the
        // first signal sets it.
        signal_hook::flag::register_conditional_shutdown(signal, 1, Arc::clone(&stop))?;
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }
    Ok(stop)
}

/// Runs one pass and logs it; returns how soon the next pass is wanted.
fn run_pass(
    store_root: &Path,
    detection: &Detection,
    options: PassOptions,
    stop: &AtomicBool,
) -> Option<Duration> {
    let hashing = |path: &Path, size: u64| {
        log(format!(
            "hashing {} ({})",
            path.display(),
            human_bytes(size)
        ));
    };
    match reconcile::run(store_root, detection, options, stop, hashing) {
        Ok(None) => None, // stopped while waiting for the store lock
        Ok(Some(report)) => {
            log_details(&report);
            log(summarize(&report));
            report.sync.next_settle().map(|wait| wait + SETTLE_MARGIN)
        }
        Err(error) => {
            log(format!("pass failed: {error}"));
            Some(RETRY_AFTER_ERROR)
        }
    }
}

fn log_details(report: &PassReport) {
    for deferred in &report.sync.deferred {
        log(format!(
            "settling {} (ready in {})",
            deferred.path.display(),
            human_duration(deferred.settles_in)
        ));
    }
    for skipped in &report.sync.skipped {
        log(format!(
            "skip {} ({})",
            skipped.path.display(),
            skipped.reason
        ));
    }
    for warning in report.sync.warnings.iter().chain(&report.warnings) {
        log(format!("warning: {warning}"));
    }
    let Some(consolidation) = &report.consolidation else {
        for planned in &report.planned {
            log(format!(
                "would clone {} ({})",
                planned.victim.display(),
                human_bytes(planned.size)
            ));
        }
        return;
    };
    for path in &consolidation.completed {
        log(format!("cloned {}", path.display()));
    }
    for refusal in &consolidation.refused {
        log(format!(
            "refused {} ({})",
            refusal.victim.display(),
            refusal.reason
        ));
    }
}

/// One-line outcome of a pass.
fn summarize(report: &PassReport) -> String {
    let mut parts = Vec::new();
    if report.sync.imported > 0 {
        parts.push(format!("{} new blob(s)", report.sync.imported));
    }
    match &report.consolidation {
        Some(done) => {
            if !done.completed.is_empty() {
                parts.push(format!(
                    "{} clone(s) made ({} freed)",
                    done.completed.len(),
                    human_bytes(done.bytes_affected)
                ));
            }
            if !done.refused.is_empty() {
                parts.push(format!("{} refused", done.refused.len()));
            }
        }
        None if !report.planned.is_empty() => {
            let bytes = report.planned.iter().map(|planned| planned.size).sum();
            parts.push(format!(
                "{} clone(s) planned ({})",
                report.planned.len(),
                human_bytes(bytes)
            ));
        }
        None => {}
    }
    if !report.sync.deferred.is_empty() {
        parts.push(format!("{} settling", report.sync.deferred.len()));
    }
    if !report.sync.skipped.is_empty() {
        parts.push(format!("{} skipped", report.sync.skipped.len()));
    }
    if parts.is_empty() {
        return "pass: nothing new".to_string();
    }
    format!("pass: {}", parts.join(", "))
}

/// Compact human duration: `45s`, `4m 59s`, `2h 5m`.
fn human_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m {}s", secs / 60, secs % 60);
    }
    format!("{}h {}m", secs / 3600, secs % 3600 / 60)
}

/// Formats `time` as an RFC 3339 UTC timestamp, second precision.
///
/// Date conversion is Howard Hinnant's `civil_from_days`, restricted to
/// post-1970 times so every intermediate stays unsigned.
fn utc_timestamp(time: SystemTime) -> String {
    let secs = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (days, of_day) = (secs / 86_400, secs % 86_400);
    let shifted = days + 719_468; // days since 0000-03-01
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153; // 0 = March
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = era * 400 + year_of_era + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// Lock held for a daemon's lifetime so two daemons never watch one store.
fn try_claim_instance(store_root: &Path) -> std::io::Result<Option<std::fs::File>> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store_root.join("daemon.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Waits until this process is the store's only daemon, or a stop signal.
///
/// A second daemon (say, a terminal run while the login agent is up) takes
/// over when the first exits instead of duplicating its passes.
fn claim_instance(store_root: &Path, stop: &AtomicBool) -> std::io::Result<Option<std::fs::File>> {
    let mut announced = false;
    while !stop.load(Ordering::Relaxed) {
        if let Some(claim) = try_claim_instance(store_root)? {
            return Ok(Some(claim));
        }
        if !announced {
            log("another modeld daemon is running; waiting to take over");
            announced = true;
        }
        std::thread::sleep(TICK);
    }
    Ok(None)
}

/// Writes one timestamped line to stdout, the log file under launchd.
///
/// A write error (say, a foreground daemon piped into a reader that quit) is
/// dropped: losing a log line must never stop a pass.
fn log(message: impl std::fmt::Display) {
    let _ = writeln!(
        std::io::stdout(),
        "{} {message}",
        utc_timestamp(SystemTime::now())
    );
}

/// The watcher plus the set of scan roots it currently follows.
struct Watches {
    watcher: notify::RecommendedWatcher,
    roots: BTreeSet<PathBuf>,
}

impl Watches {
    fn new(watcher: notify::RecommendedWatcher) -> Self {
        Self {
            watcher,
            roots: BTreeSet::new(),
        }
    }

    /// Watches the store directory itself, for `config.toml` edits.
    fn watch_store(&mut self, store_root: &Path) -> notify::Result<()> {
        use notify::Watcher as _;
        self.watcher
            .watch(store_root, notify::RecursiveMode::NonRecursive)
    }

    /// Re-arms watches so exactly the currently detected roots are followed.
    ///
    /// A root that cannot be watched is logged and retried on the next pass.
    fn follow(&mut self, roots: &[ProviderRoot]) {
        use notify::Watcher as _;
        let wanted: BTreeSet<PathBuf> = roots.iter().map(|root| root.root.clone()).collect();
        for gone in self.roots.difference(&wanted) {
            let _ = self.watcher.unwatch(gone);
            log(format!("unwatching {}", gone.display()));
        }
        self.roots.retain(|root| wanted.contains(root));
        for root in wanted {
            if self.roots.contains(&root) {
                continue;
            }
            match self.watcher.watch(&root, notify::RecursiveMode::Recursive) {
                Ok(()) => {
                    log(format!("watching {}", root.display()));
                    self.roots.insert(root);
                }
                Err(error) => log(format!("cannot watch {} ({error})", root.display())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{Deferred, SyncReport};
    use modeld_core::consolidate::{Refusal, Replacement, Report};

    fn report(planned: usize, consolidation: Option<Report>) -> PassReport {
        let digest =
            modeld_core::Digest::new(modeld_core::Algorithm::Sha256, vec![1; 32]).expect("digest");
        PassReport {
            sync: SyncReport {
                imported: 1,
                deferred: vec![Deferred {
                    path: PathBuf::from("/m/new.gguf"),
                    settles_in: Duration::from_secs(90),
                }],
                ..SyncReport::default()
            },
            planned: (0..planned)
                .map(|_| Replacement {
                    canonical: PathBuf::from("/blob"),
                    victim: PathBuf::from("/m/copy.gguf"),
                    digest: digest.clone(),
                    size: 2_600_000_000,
                })
                .collect(),
            consolidation,
            warnings: vec![],
        }
    }

    #[test]
    fn summary_counts_clones_and_freed_bytes() {
        let done = Report {
            completed: vec![PathBuf::from("/m/copy.gguf")],
            refused: vec![Refusal {
                victim: PathBuf::from("/m/busy.gguf"),
                reason: "open writer".to_string(),
            }],
            bytes_affected: 2_600_000_000,
        };

        assert_eq!(
            summarize(&report(2, Some(done))),
            "pass: 1 new blob(s), 1 clone(s) made (2.6 GB freed), 1 refused, 1 settling"
        );
    }

    #[test]
    fn dry_run_summary_reports_planned_clones() {
        assert_eq!(
            summarize(&report(1, None)),
            "pass: 1 new blob(s), 1 clone(s) planned (2.6 GB), 1 settling"
        );
    }

    #[test]
    fn idle_summary_is_short() {
        let idle = PassReport {
            sync: SyncReport::default(),
            planned: vec![],
            consolidation: None,
            warnings: vec![],
        };
        assert_eq!(summarize(&idle), "pass: nothing new");
    }

    #[test]
    fn second_daemon_cannot_claim_a_store_until_the_first_exits() {
        let dir = tempfile::tempdir().expect("create temp dir");

        let first = try_claim_instance(dir.path()).expect("claim");
        assert!(first.is_some());
        assert!(try_claim_instance(dir.path()).expect("claim").is_none());
        drop(first);

        assert!(try_claim_instance(dir.path()).expect("claim").is_some());
    }

    #[test]
    fn durations_read_at_a_glance() {
        assert_eq!(human_duration(Duration::from_secs(45)), "45s");
        assert_eq!(human_duration(Duration::from_secs(299)), "4m 59s");
        assert_eq!(human_duration(Duration::from_mins(125)), "2h 5m");
    }

    #[test]
    fn timestamps_are_utc_rfc3339() {
        let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(utc_timestamp(at(0)), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(at(951_782_400)), "2000-02-29T00:00:00Z");
        assert_eq!(utc_timestamp(at(1_000_000_000)), "2001-09-09T01:46:40Z");
        assert_eq!(utc_timestamp(at(1_790_000_000)), "2026-09-21T14:13:20Z");
    }
}
