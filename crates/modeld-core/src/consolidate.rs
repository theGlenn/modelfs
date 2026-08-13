//! Safe duplicate consolidation: replace byte-identical files with APFS clones.
//!
//! Protocol per replacement (see DECISIONS.md, "Conservative dedupe protocol"):
//!
//! 1. Fully hash the canonical file; require the expected digest.
//! 2. Snapshot the victim's stat (size, mtime, mode, inode); refuse recently
//!    written files.
//! 3. Fully hash the victim; require the same digest; re-stat to detect writes
//!    that raced the hash.
//! 4. Clone canonical to a temp name in the victim's directory, re-stat once more,
//!    then atomically swap temp and victim (`renamex_np` + `RENAME_SWAP`).
//! 5. Post-verify the swapped-in file by hashing it again; on mismatch swap back.
//! 6. Restore the victim's original mode and mtime (providers key caches on mtime),
//!    release the old bytes, journal the replacement.
//!
//! Every replacement is journaled so [`restore`] can rebuild fully independent
//! copies later (undoing the space saving, returning to the pre-dedupe state).

use crate::apfs;
use crate::digest::Digest;
use serde::{Deserialize, Serialize};
use std::fs::Metadata;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Refuse to touch files written within this window.
///
/// A file this fresh may still be mid-download by a provider that does not use
/// `.part`/`.incomplete` staging names. Five minutes comfortably exceeds observed
/// provider write patterns while barely delaying dedupe of settled files.
pub const RECENT_WRITE_WINDOW: Duration = Duration::from_mins(5);

/// One planned replacement: make `victim` an APFS clone of `canonical`.
#[derive(Debug, Clone)]
pub struct Replacement {
    pub canonical: PathBuf,
    pub victim: PathBuf,
    /// Expected content digest of BOTH files, verified before and after the swap.
    pub digest: Digest,
    pub size: u64,
}

/// A replacement that was not performed, and why.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub victim: PathBuf,
    pub reason: String,
}

/// What a consolidation or restore run actually did.
#[derive(Debug, Default)]
pub struct Report {
    pub completed: Vec<PathBuf>,
    pub refused: Vec<Refusal>,
    pub bytes_affected: u64,
}

/// Journaled record of one performed replacement, enough to restore it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub victim: PathBuf,
    pub canonical: PathBuf,
    pub digest: Digest,
    pub mode: u32,
    pub mtime_epoch_secs: u64,
    /// Sub-second part of the original mtime. LM Studio keys metadata caches on
    /// millisecond mtimes, so second precision is not enough.
    pub mtime_nanos: u32,
    pub replaced_at_epoch_secs: u64,
}

/// Append-only JSONL journal of performed replacements.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    /// Opens (creating parents if needed) the journal at `path`.
    ///
    /// # Errors
    /// I/O error creating the parent directory.
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self { path })
    }

    /// Reads all entries; a missing journal file is an empty journal.
    ///
    /// # Errors
    /// I/O error reading the file, or a corrupt (unparseable) line.
    pub fn entries(&self) -> std::io::Result<Vec<JournalEntry>> {
        let contents = match std::fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(std::io::Error::other))
            .collect()
    }

    /// Appends one entry, flushed before returning.
    ///
    /// # Errors
    /// I/O error opening, writing, or flushing the journal file.
    pub fn append(&self, entry: &JournalEntry) -> std::io::Result<()> {
        let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{line}")?;
        file.sync_all()
    }

    /// Replaces the journal contents with `entries`.
    ///
    /// # Errors
    /// I/O error writing the journal file.
    pub fn rewrite(&self, entries: &[JournalEntry]) -> std::io::Result<()> {
        let mut lines = String::new();
        for entry in entries {
            lines.push_str(&serde_json::to_string(entry).map_err(std::io::Error::other)?);
            lines.push('\n');
        }
        std::fs::write(&self.path, lines)
    }
}

/// Executes planned replacements, journaling each success.
///
/// Failures are per-replacement [`Refusal`]s, never aborts: one busy file must not
/// prevent consolidating the rest.
#[must_use]
pub fn consolidate(replacements: &[Replacement], journal: &Journal) -> Report {
    let mut report = Report::default();
    for replacement in replacements {
        match replace_with_clone(replacement) {
            Ok(entry) => {
                if let Err(error) = journal.append(&entry) {
                    report.refused.push(Refusal {
                        victim: replacement.victim.clone(),
                        reason: format!("replaced but journal write failed: {error}"),
                    });
                    continue;
                }
                report.completed.push(replacement.victim.clone());
                report.bytes_affected += replacement.size;
            }
            Err(reason) => report.refused.push(Refusal {
                victim: replacement.victim.clone(),
                reason,
            }),
        }
    }
    report
}

/// Rebuilds independent copies for all journaled replacements (undoes dedupe).
///
/// Entries whose file content changed since the replacement are left untouched and
/// kept in the journal; everything successfully restored is removed from it.
///
/// # Errors
/// I/O error reading or rewriting the journal itself; per-file failures are
/// reported as refusals, not errors.
pub fn restore(journal: &Journal) -> std::io::Result<Report> {
    let mut report = Report::default();
    let mut remaining = Vec::new();
    for entry in journal.entries()? {
        match restore_entry(&entry) {
            Ok(size) => {
                report.completed.push(entry.victim.clone());
                report.bytes_affected += size;
            }
            Err(reason) => {
                report.refused.push(Refusal {
                    victim: entry.victim.clone(),
                    reason,
                });
                remaining.push(entry);
            }
        }
    }
    journal.rewrite(&remaining)?;
    Ok(report)
}

/// Stat snapshot used to detect concurrent modification of the victim.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct StatSnapshot {
    size: u64,
    mtime_epoch_secs: u64,
    mtime_nanos: u32,
    inode: u64,
    mode: u32,
}

impl StatSnapshot {
    fn of(path: &Path) -> Result<Self, String> {
        let metadata =
            std::fs::symlink_metadata(path).map_err(|error| format!("cannot stat: {error}"))?;
        if !metadata.is_file() {
            return Err("not a regular file".to_string());
        }
        Ok(Self::from_metadata(&metadata))
    }

    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            size: metadata.len(),
            mtime_epoch_secs: u64::try_from(metadata.mtime()).unwrap_or(0),
            mtime_nanos: u32::try_from(metadata.mtime_nsec()).unwrap_or(0),
            inode: metadata.ino(),
            mode: metadata.mode(),
        }
    }

    fn modified_within(&self, window: Duration) -> bool {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.mtime_epoch_secs) < window.as_secs()
    }
}

fn replace_with_clone(replacement: &Replacement) -> Result<JournalEntry, String> {
    verify_digest(&replacement.canonical, &replacement.digest, "canonical")?;

    let before = StatSnapshot::of(&replacement.victim)?;
    if before.modified_within(RECENT_WRITE_WINDOW) {
        return Err("recently written; may still be downloading".to_string());
    }
    verify_digest(&replacement.victim, &replacement.digest, "victim")?;
    ensure_unchanged(&replacement.victim, before, "during hashing")?;

    let tmp = temp_sibling(&replacement.victim)?;
    apfs::clone_file(&replacement.canonical, &tmp)
        .map_err(|error| format!("clonefile failed: {error}"))?;
    if let Err(reason) = ensure_unchanged(&replacement.victim, before, "after cloning") {
        let _ = std::fs::remove_file(&tmp);
        return Err(reason);
    }
    if let Err(error) = apfs::swap_files(&tmp, &replacement.victim) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("atomic swap failed: {error}"));
    }

    // The victim's original bytes now live at `tmp` — instant rollback until released.
    if let Err(reason) = verify_digest(&replacement.victim, &replacement.digest, "post-swap") {
        let rollback = apfs::swap_files(&tmp, &replacement.victim);
        let _ = std::fs::remove_file(&tmp);
        return Err(match rollback {
            Ok(()) => format!("{reason}; rolled back"),
            Err(error) => format!("{reason}; ROLLBACK FAILED: {error}"),
        });
    }

    restore_file_attributes(
        &replacement.victim,
        before.mode,
        before.mtime_epoch_secs,
        before.mtime_nanos,
    );
    let _ = std::fs::remove_file(&tmp); // release the old bytes — this frees the space
    Ok(JournalEntry {
        victim: replacement.victim.clone(),
        canonical: replacement.canonical.clone(),
        digest: replacement.digest.clone(),
        mode: before.mode,
        mtime_epoch_secs: before.mtime_epoch_secs,
        mtime_nanos: before.mtime_nanos,
        replaced_at_epoch_secs: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    })
}

fn restore_entry(entry: &JournalEntry) -> Result<u64, String> {
    verify_digest(&entry.victim, &entry.digest, "journaled file")?;
    let size = StatSnapshot::of(&entry.victim)?.size;

    let tmp = temp_sibling(&entry.victim)?;
    if let Err(error) = copy_independent(&entry.victim, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("independent copy failed: {error}"));
    }
    if let Err(error) = apfs::swap_files(&tmp, &entry.victim) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("atomic swap failed: {error}"));
    }
    let _ = std::fs::remove_file(&tmp); // release the clone
    restore_file_attributes(
        &entry.victim,
        entry.mode,
        entry.mtime_epoch_secs,
        entry.mtime_nanos,
    );
    Ok(size)
}

/// Byte-by-byte copy that cannot share extents with the source.
///
/// `std::fs::copy` uses `copyfile` cloning on APFS, which would silently recreate
/// the sharing that restore exists to undo.
fn copy_independent(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut reader = std::fs::File::open(src)?;
    let mut writer = std::fs::File::create(dst)?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buffer[..n])?;
    }
    writer.sync_all()
}

fn verify_digest(path: &Path, expected: &Digest, role: &str) -> Result<(), String> {
    let actual =
        Digest::sha256_file(path).map_err(|error| format!("cannot hash {role}: {error}"))?;
    if actual == *expected {
        Ok(())
    } else {
        Err(format!("{role} content changed (digest mismatch)"))
    }
}

fn ensure_unchanged(path: &Path, before: StatSnapshot, when: &str) -> Result<(), String> {
    let now = StatSnapshot::of(path)?;
    if now == before {
        Ok(())
    } else {
        Err(format!("file changed {when}; aborted untouched"))
    }
}

fn temp_sibling(path: &Path) -> Result<PathBuf, String> {
    let parent = path.parent().ok_or("path has no parent directory")?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("artifact");
    Ok(parent.join(format!(".{name}.modeld-tmp-{}", std::process::id())))
}

fn restore_file_attributes(path: &Path, mode: u32, mtime_epoch_secs: u64, mtime_nanos: u32) {
    // Best-effort: attribute restoration failing must not fail a verified swap.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    if let Ok(file) = std::fs::File::open(path) {
        let mtime = SystemTime::UNIX_EPOCH + Duration::new(mtime_epoch_secs, mtime_nanos);
        let _ = file.set_times(std::fs::FileTimes::new().set_modified(mtime));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds two identical settled files plus a journal, returns (dir, plan, journal).
    fn fixture() -> (tempfile::TempDir, Replacement, Journal) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let canonical = dir.path().join("canonical.gguf");
        let victim = dir.path().join("victim.gguf");
        std::fs::write(&canonical, b"identical model bytes").expect("write canonical");
        std::fs::write(&victim, b"identical model bytes").expect("write victim");
        age(&canonical);
        age(&victim);
        let digest = Digest::sha256_file(&canonical).expect("hash");
        let journal = Journal::open(dir.path().join("journal.jsonl")).expect("open journal");
        let replacement = Replacement {
            canonical,
            victim,
            digest,
            size: 21,
        };
        (dir, replacement, journal)
    }

    /// Backdates a file past `RECENT_WRITE_WINDOW` so the freshness guard passes.
    fn age(path: &Path) {
        let old = SystemTime::now() - Duration::from_hours(1);
        let file = std::fs::File::open(path).expect("open");
        file.set_times(std::fs::FileTimes::new().set_modified(old))
            .expect("set mtime");
    }

    #[test]
    fn consolidates_identical_files_and_journals_the_swap() {
        let (_dir, replacement, journal) = fixture();
        let victim_mtime = std::fs::metadata(&replacement.victim)
            .expect("stat")
            .modified()
            .expect("mtime");

        let report = consolidate(std::slice::from_ref(&replacement), &journal);

        assert_eq!(report.completed, vec![replacement.victim.clone()]);
        assert!(report.refused.is_empty());
        assert_eq!(report.bytes_affected, 21);
        assert_eq!(
            std::fs::read(&replacement.victim).expect("read"),
            b"identical model bytes"
        );
        // mtime preserved (LM Studio keys metadata caches on it)
        let restored_mtime = std::fs::metadata(&replacement.victim)
            .expect("stat")
            .modified()
            .expect("mtime");
        assert_eq!(restored_mtime, victim_mtime);
        assert_eq!(journal.entries().expect("entries").len(), 1);
    }

    #[test]
    fn refuses_recently_written_victim() {
        let (_dir, mut replacement, journal) = fixture();
        std::fs::write(&replacement.victim, b"identical model bytes").expect("touch");
        replacement.digest = Digest::sha256_file(&replacement.canonical).expect("hash");

        let report = consolidate(std::slice::from_ref(&replacement), &journal);

        assert!(report.completed.is_empty());
        assert!(report.refused[0].reason.contains("recently written"));
    }

    #[test]
    fn refuses_victim_with_different_content() {
        let (_dir, replacement, journal) = fixture();
        std::fs::write(&replacement.victim, b"drifted content bytes").expect("mutate");
        age(&replacement.victim);

        let report = consolidate(std::slice::from_ref(&replacement), &journal);

        assert!(report.completed.is_empty());
        assert!(report.refused[0].reason.contains("digest mismatch"));
        assert_eq!(
            std::fs::read(&replacement.victim).expect("read"),
            b"drifted content bytes"
        );
    }

    #[test]
    fn restore_rebuilds_independent_copy_and_clears_journal() {
        let (_dir, replacement, journal) = fixture();
        let _ = consolidate(std::slice::from_ref(&replacement), &journal);
        let inode_before = std::fs::metadata(&replacement.victim).expect("stat").ino();

        let report = restore(&journal).expect("restore");

        assert_eq!(report.completed, vec![replacement.victim.clone()]);
        assert_eq!(
            std::fs::read(&replacement.victim).expect("read"),
            b"identical model bytes"
        );
        assert!(journal.entries().expect("entries").is_empty());
        // New inode: restore rebuilt the file rather than leaving the clone in place.
        let inode_after = std::fs::metadata(&replacement.victim).expect("stat").ino();
        assert_ne!(inode_before, inode_after);
    }

    #[test]
    fn restore_skips_files_changed_since_dedupe_and_keeps_journal_entry() {
        let (_dir, replacement, journal) = fixture();
        let _ = consolidate(std::slice::from_ref(&replacement), &journal);
        std::fs::write(&replacement.victim, b"user replaced this file").expect("mutate");

        let report = restore(&journal).expect("restore");

        assert!(report.completed.is_empty());
        assert_eq!(report.refused.len(), 1);
        assert_eq!(journal.entries().expect("entries").len(), 1);
        assert_eq!(
            std::fs::read(&replacement.victim).expect("read"),
            b"user replaced this file"
        );
    }
}
