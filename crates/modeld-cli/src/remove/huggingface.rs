//! Removing Hugging Face hub-cache files.
//!
//! The hub cache is a download cache, so files are deleted, not trashed.
//! Snapshot links go before the blob they point at, so no link ever
//! dangles; `snapshots/<commit>` and `refs/` stay. Nothing is touched while
//! `huggingface_hub` holds the blob's download lock, and the lock is held by
//! modeld while it deletes, so no download can start meanwhile. The repo
//! folder goes too once no tracked model file (1 MiB or more, or still
//! downloading) would remain in it.

use super::{Action, Action::DeleteFolder, Collateral, Draft, RefusalCode, Removal, Step, Target};
use modeld_providers::huggingface::{leftovers, links_to, lock_file, repo_of_blob};
use modeld_providers::scan::DEFAULT_MIN_SIZE;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

/// Plans removing hub-cache blobs, their links, and emptied repo folders.
pub(super) fn plan(targets: Option<&Vec<Target>>, draft: &mut Draft) {
    let Some(targets) = targets else { return };
    let mut by_repo: BTreeMap<PathBuf, Vec<&Target>> = BTreeMap::new();
    for target in targets {
        let repo = target.reference.path.parent().and_then(Path::parent);
        by_repo
            .entry(repo.unwrap_or(Path::new("/")).to_path_buf())
            .or_default()
            .push(target);
    }
    for (repo, targets) in by_repo {
        let mut removing = Vec::new();
        for target in targets.iter().copied() {
            if plan_blob(target, draft) {
                removing.push(target.reference.path.clone());
            }
        }
        if let Some(target) = targets.first().filter(|_| !removing.is_empty()) {
            plan_repo_folder(&repo, &removing, target, draft);
        }
    }
}

/// Plans one blob and its links; returns false when refused.
fn plan_blob(target: &Target, draft: &mut Draft) -> bool {
    let blob = &target.reference.path;
    if is_downloading(blob) {
        draft.refuse(
            RefusalCode::DownloadInProgress,
            blob,
            "Hugging Face is downloading it",
        );
        return false;
    }
    for link in links_to(blob) {
        let destination = std::fs::read_link(&link).unwrap_or_default();
        draft.fingerprint(format!("link|{}|{}", link.display(), destination.display()));
        draft.step(target, Action::Unlink, &link);
    }
    draft.step(target, Action::Delete, blob);
    draft.removals.insert(blob.clone(), Removal::Delete);
    true
}

/// Plans deleting the repo folder when nothing tracked would remain.
fn plan_repo_folder(repo: &Path, removing: &[PathBuf], target: &Target, draft: &mut Draft) {
    let left = leftovers(repo, removing);
    let keeps_a_model = left.iter().any(|(path, size)| {
        *size >= DEFAULT_MIN_SIZE || path.extension().is_some_and(|ext| ext == "incomplete")
    });
    if keeps_a_model {
        return;
    }
    let name = repo_of_blob(&target.reference.path).unwrap_or_else(|| repo.display().to_string());
    for (path, size) in left {
        draft.collateral.push((
            Collateral {
                path,
                size,
                reason: format!("rest of the {name} download"),
            },
            Removal::Delete,
        ));
    }
    draft.step(target, DeleteFolder, repo);
}

/// Whether a download of `blob` is under way: lock held, or partial file.
fn is_downloading(blob: &Path) -> bool {
    let mut partial = blob.as_os_str().to_owned();
    partial.push(".incomplete");
    if Path::new(&partial).exists() {
        return true;
    }
    lock_file(blob)
        .filter(|lock| lock.exists())
        .is_some_and(|lock| lock_is_held(&lock))
}

/// Tries the download lock; holding it means a download is running.
fn lock_is_held(lock: &Path) -> bool {
    let Ok(file) = File::open(lock) else {
        return false;
    };
    matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// Takes the download lock of every blob the steps delete.
///
/// Held while the steps run, so `huggingface_hub` cannot start writing a
/// blob modeld is deleting. Only existing lock files are used.
///
/// # Errors
/// A download took a lock since the plan was made.
pub(super) fn hold_locks(steps: &[Step]) -> Result<Vec<File>, String> {
    let mut held = Vec::new();
    let blobs = steps
        .iter()
        .filter(|step| step.provider == "huggingface" && step.action == Action::Delete);
    for step in blobs {
        let Some(lock) = lock_file(&step.path).filter(|lock| lock.exists()) else {
            continue;
        };
        let Ok(file) = File::open(&lock) else {
            continue;
        };
        if file.try_lock().is_err() {
            return Err(format!(
                "a download of {} started; nothing was removed",
                step.path.display()
            ));
        }
        held.push(file);
    }
    Ok(held)
}
