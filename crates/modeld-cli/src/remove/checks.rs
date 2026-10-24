//! The checks every path passes before a plan may remove it.

use super::{Note, NoteCode, Refusal, RefusalCode};
use crate::inventory;
use modeld_core::Digest;
use modeld_providers::ProviderRoot;
use modeld_store::{FileStamp, Reference, Store};
use std::path::Path;
use std::process::Command;

/// What the checks need besides the path.
#[derive(Debug)]
pub(super) struct Context<'a> {
    pub store: &'a Store,
    pub roots: &'a [ProviderRoot],
    /// Hash a file whose digest is not cached for its current stamp.
    pub verify: bool,
}

/// A path that passed every check.
#[derive(Debug)]
pub(super) struct Checked {
    /// The scanned folder the path is in.
    pub root: ProviderRoot,
    /// The stamp the file had when it passed.
    pub stamp: FileStamp,
    /// The path's stamp, for the plan id.
    pub fingerprint: String,
    /// Set when the content still has to be proven by hashing.
    pub note: Option<Note>,
}

/// Checks that `reference` may be removed as a copy of `digest`.
///
/// The path must be a regular file inside a folder modeld scans for the
/// reference's app, hold the model's bytes, and be open in no process.
///
/// # Errors
/// The first check that fails, as a refusal.
pub(super) fn check(
    context: &Context<'_>,
    digest: &Digest,
    reference: &Reference,
    on_hashing: &mut dyn FnMut(&Path, u64),
) -> Result<Checked, Refusal> {
    let path = &reference.path;
    let refuse = |code: RefusalCode, message: String| Refusal {
        code,
        path: path.clone(),
        message,
        processes: Vec::new(),
    };
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        refuse(
            RefusalCode::Unreadable,
            format!("cannot check it ({error})"),
        )
    })?;
    if !metadata.is_file() {
        return Err(refuse(
            RefusalCode::NotRegularFile,
            "not a regular file anymore".to_string(),
        ));
    }
    if inside_store(context.store, path) {
        return Err(refuse(
            RefusalCode::OutsideRoots,
            "inside the ModelFS store; only the store manages its files".to_string(),
        ));
    }
    let root = root_of(context.roots, reference).ok_or_else(|| {
        refuse(
            RefusalCode::OutsideRoots,
            "outside every folder modeld scans now".to_string(),
        )
    })?;
    let stamp = FileStamp::from_metadata(&metadata);
    let note = prove_content(context, digest, path, stamp, on_hashing)
        .map_err(|message| refuse(RefusalCode::ContentChanged, message))?;
    let processes = processes_using(path).map_err(|message| refuse(RefusalCode::InUse, message))?;
    if !processes.is_empty() {
        return Err(Refusal {
            code: RefusalCode::InUse,
            path: path.clone(),
            message: format!("open in {}", processes.join(", ")),
            processes,
        });
    }
    Ok(Checked {
        root: root.clone(),
        stamp,
        fingerprint: format!(
            "stamp|{}|{}|{}|{}.{}|{}.{}",
            path.display(),
            stamp.inode,
            stamp.size,
            stamp.mtime_secs,
            stamp.mtime_nanos,
            stamp.ctime_secs,
            stamp.ctime_nanos
        ),
        note,
    })
}

/// Whether `path` is the store's own (its blobs, its registry).
fn inside_store(store: &Store, path: &Path) -> bool {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.starts_with(store.root()) || canonical(path).starts_with(canonical(store.root()))
}

/// The most specific scanned folder of the reference's app holding it.
fn root_of<'a>(roots: &'a [ProviderRoot], reference: &Reference) -> Option<&'a ProviderRoot> {
    roots
        .iter()
        .filter(|root| inventory::provider_name(root.kind) == reference.provider)
        .filter(|root| reference.path.starts_with(&root.root))
        .max_by_key(|root| root.root.as_os_str().len())
}

/// Proves `path` still holds `digest`: a cache hit, or a hash when verifying.
///
/// Without `verify` (a dry run), a cache miss becomes a note: the file will
/// be hashed under the lock before anything is removed.
fn prove_content(
    context: &Context<'_>,
    digest: &Digest,
    path: &Path,
    stamp: FileStamp,
    on_hashing: &mut dyn FnMut(&Path, u64),
) -> Result<Option<Note>, String> {
    let cached = context.store.cached_digest(path, stamp).ok().flatten();
    if cached.as_ref() == Some(digest) {
        return Ok(None);
    }
    if !context.verify {
        return Ok(Some(Note {
            code: NoteCode::VerifyByHashing,
            path: path.to_path_buf(),
            message: "changed since modeld last read it; it is hashed again before removal"
                .to_string(),
            paths: Vec::new(),
        }));
    }
    on_hashing(path, stamp.size);
    let actual = Digest::sha256_file(path).map_err(|error| format!("cannot hash it ({error})"))?;
    if !FileStamp::of(path).is_ok_and(|now| now == stamp) {
        return Err("changed while it was being hashed".to_string());
    }
    if actual != *digest {
        return Err("no longer holds this model's bytes".to_string());
    }
    let _ = context.store.remember_digest(path, stamp, digest);
    Ok(None)
}

/// The names of every process that has `path` open, through `lsof`.
///
/// # Errors
/// `lsof` could not run or failed; the caller treats that as "in use".
pub(super) fn processes_using(path: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("/usr/sbin/lsof")
        .arg("-Fc")
        .arg("--")
        .arg(path)
        .output()
        .map_err(|error| format!("cannot check whether it is open ({error})"))?;
    // lsof exits 1 when no process has the file open.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(format!(
            "cannot check whether it is open (lsof exited {})",
            output.status
        ));
    }
    let mut names: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('c'))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_file_names_its_process() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("model.gguf");
        std::fs::write(&path, b"weights").expect("write");

        let closed = processes_using(&path).expect("lsof");
        let _open = std::fs::File::open(&path).expect("open");
        let open = processes_using(&path).expect("lsof");

        assert!(closed.is_empty());
        assert_eq!(open.len(), 1, "{open:?}");
    }
}
