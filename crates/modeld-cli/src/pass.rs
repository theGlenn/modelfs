//! `modeld pass`: run one daemon pass now, in the foreground.
//!
//! The pass is exactly what the daemon runs: it syncs every root, leaves
//! files written in the last five minutes for later, and clones duplicates
//! of stored blobs. It is how the Mac app deduplicates on demand, and with
//! `--dry-run` how it fills the store without touching any app's files.
//!
//! The first SIGINT or SIGTERM stops a pass still waiting for the store
//! lock; once it holds the lock (the `locked` event) the pass runs to the
//! end, so no swap is cut short. A second signal exits at once.

use crate::Output;
use crate::json::{self, ErrorCode};
use crate::reconcile::{self, PassEvent, PassOptions};
use crate::report::human_bytes;
use std::path::Path;

/// Runs one pass against the store at `store_root`; exits 1 on failure.
pub fn run(store_root: &Path, options: PassOptions, output: Output) {
    let stop = match crate::daemon::stop_flag() {
        Ok(stop) => stop,
        Err(error) => crate::fail(
            output,
            ErrorCode::PassFailed,
            format!("cannot handle stop signals: {error}"),
        ),
    };
    let detection = modeld_providers::detect_all();
    let on_event = |event: PassEvent<'_>| match output {
        Output::Json => json::emit_event(event.into()),
        Output::Text => print_event(event),
    };
    match reconcile::run(store_root, &detection, options, &stop, on_event) {
        Ok(Some(report)) if output == Output::Json => {
            json::emit_result(json::Pass::new(&report, options.dry_run));
        }
        Ok(Some(report)) => {
            for line in crate::daemon::describe(&report) {
                println!("{line}");
            }
        }
        Ok(None) => crate::fail(
            output,
            ErrorCode::Cancelled,
            "stopped while waiting for another modeld process",
        ),
        Err(error) => crate::fail(
            output,
            ErrorCode::PassFailed,
            format!("pass failed: {error}"),
        ),
    }
}

/// Text-mode progress on stderr, like `sync` and `dedupe` print it.
fn print_event(event: PassEvent<'_>) {
    match event {
        PassEvent::WaitingForLock => {
            eprintln!("waiting for another modeld process (daemon pass?) to finish…");
        }
        PassEvent::Locked => {}
        PassEvent::Hashing { path, size } => {
            eprintln!("hashing {} ({})", path.display(), human_bytes(size));
        }
        PassEvent::Cloning { path, size } => {
            eprintln!("cloning {} ({})", path.display(), human_bytes(size));
        }
    }
}
