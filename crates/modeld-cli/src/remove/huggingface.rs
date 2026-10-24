//! Removing Hugging Face hub-cache files.
//!
//! The hub cache is a download cache, so files are deleted, not trashed.
//! Snapshot links go before the blob they point at, so no link ever
//! dangles; `snapshots/<commit>` and `refs/` stay. Nothing is touched while
//! `huggingface_hub` holds the blob's download lock, and the lock is held by
//! modeld while it deletes, so no download can start meanwhile. The repo
//! folder goes too once no tracked model file (1 MiB or more, or still
//! downloading) would remain in it.

use super::{Action, Collateral, Draft, RefusalCode, Removal, Step, Target};
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
        match repo_folder(target) {
            Some(repo) => by_repo.entry(repo).or_default().push(target),
            None => draft.refuse(
                RefusalCode::OutsideRoots,
                &target.reference.path,
                "not laid out like the Hugging Face cache (models--<org>--<name>/blobs/<file>)",
            ),
        }
    }
    for (repo, targets) in by_repo {
        let mut removing = Vec::new();
        let mut unlinking = Vec::new();
        for target in targets {
            if let Some(links) = plan_blob(target, draft) {
                removing.push(target);
                unlinking.extend(links);
            }
        }
        if !removing.is_empty() {
            plan_repo_folder(&repo, &removing, &unlinking, draft);
        }
    }
}

/// The `models--<org>--<name>` folder of a blob, if the layout is the hub's.
///
/// The folder must sit directly in the scanned hub root, so a stray
/// registry row can never make modeld delete anything above a repo.
fn repo_folder(target: &Target) -> Option<PathBuf> {
    let blobs = target.reference.path.parent()?;
    let repo = blobs.parent()?;
    let named_right =
        blobs.file_name()? == "blobs" && repo.file_name()?.to_str()?.starts_with("models--");
    (named_right && repo.parent()? == target.root.root).then(|| repo.to_path_buf())
}

/// Plans one blob and its links; returns the links, or `None` when refused.
fn plan_blob(target: &Target, draft: &mut Draft) -> Option<Vec<PathBuf>> {
    let blob = &target.reference.path;
    if is_downloading(blob) {
        draft.refuse(
            RefusalCode::DownloadInProgress,
            blob,
            "Hugging Face is downloading it",
        );
        return None;
    }
    let links = links_to(blob);
    for link in &links {
        let destination = std::fs::read_link(link).unwrap_or_default();
        draft.fingerprint(format!("link|{}|{}", link.display(), destination.display()));
        draft.step(target, Action::Unlink, link, vec![target.guard()]);
    }
    draft.step(target, Action::Delete, blob, vec![target.guard()]);
    draft.removals.insert(blob.clone(), Removal::Delete);
    Some(links)
}

/// Plans deleting the repo folder when nothing tracked would remain.
///
/// Everything else in the folder (configs, other links, `refs/`) is listed
/// as going along. If the folder cannot be fully read, it stays.
fn plan_repo_folder(repo: &Path, removing: &[&Target], unlinking: &[PathBuf], draft: &mut Draft) {
    let blobs: Vec<PathBuf> = removing
        .iter()
        .map(|target| target.reference.path.clone())
        .collect();
    let keeps_a_model = leftovers(repo, &blobs).iter().any(|(path, size)| {
        *size >= DEFAULT_MIN_SIZE || path.extension().is_some_and(|ext| ext == "incomplete")
    });
    if keeps_a_model {
        return;
    }
    let Some(rest) = everything_in(repo) else {
        return;
    };
    let name =
        repo_of_blob(&removing[0].reference.path).unwrap_or_else(|| repo.display().to_string());
    for (path, size) in rest {
        if blobs.contains(&path) || unlinking.contains(&path) {
            continue;
        }
        draft.collateral.push((
            Collateral {
                path,
                size,
                reason: format!("rest of the {name} download"),
            },
            Removal::Delete,
        ));
    }
    let guards = removing.iter().map(|target| target.guard()).collect();
    draft.step(removing[0], Action::DeleteFolder, repo, guards);
}

/// Every file and link under `folder` with its size; `None` on any read error.
fn everything_in(folder: &Path) -> Option<Vec<(PathBuf, u64)>> {
    let mut found = Vec::new();
    for entry in walkdir::WalkDir::new(folder).follow_links(false) {
        let entry = entry.ok()?;
        if entry.file_type().is_dir() {
            continue;
        }
        let size = std::fs::symlink_metadata(entry.path()).ok()?.len();
        found.push((entry.into_path(), size));
    }
    found.sort();
    Some(found)
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
