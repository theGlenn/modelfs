//! The store's change counter, `~/.modeld/generation`.
//!
//! Every command that can change what `ls` shows (`sync`, `dedupe`,
//! `restore`, `gc`, and each daemon pass) rewrites this file when it
//! finishes. A viewer such as the Mac app watches this one file to know when
//! to refresh. It must not watch the registry instead: `SQLite` creates and
//! deletes `registry.db-wal` on reads too, so each refresh would trigger the
//! next one.
//!
//! The content is a number that grows with every write; only its change
//! matters. It is written to a temp file and renamed into place, so a reader
//! never sees a partial write. The daemon ignores events in the store
//! directory, so writing it never wakes the daemon.

use std::path::Path;

/// File name of the generation counter inside the store directory.
pub const FILE_NAME: &str = "generation";

/// Moves the store's generation forward, telling viewers to refresh.
///
/// The new value is the current time in nanoseconds, or one more than the
/// previous value when the clock has not moved past it.
///
/// # Errors
/// Writing or renaming the temp file failed.
pub fn bump(store_root: &Path) -> std::io::Result<()> {
    let path = store_root.join(FILE_NAME);
    let previous = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let next = modeld_store::sync_stamp().max(previous.saturating_add(1));
    let temp = store_root.join(format!(".{FILE_NAME}.tmp-{}", std::process::id()));
    std::fs::write(&temp, format!("{next}\n"))
        .and_then(|()| std::fs::rename(&temp, &path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(dir: &Path) -> u64 {
        std::fs::read_to_string(dir.join(FILE_NAME))
            .expect("read generation")
            .trim()
            .parse()
            .expect("number")
    }

    #[test]
    fn every_bump_moves_the_generation_forward() {
        let dir = tempfile::tempdir().expect("create temp dir");

        bump(dir.path()).expect("first bump");
        let first = read(dir.path());
        bump(dir.path()).expect("second bump");

        assert!(read(dir.path()) > first);
    }

    #[test]
    fn a_counter_ahead_of_the_clock_still_grows() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join(FILE_NAME), format!("{}\n", u64::MAX - 1)).expect("seed");

        bump(dir.path()).expect("bump");

        assert_eq!(read(dir.path()), u64::MAX);
    }

    #[test]
    fn bump_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("create temp dir");

        bump(dir.path()).expect("bump");

        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, [FILE_NAME]);
    }
}
