//! Removing plain files: LM Studio models and your own folders' files.
//!
//! Both go to the Trash (`/usr/bin/trash`), where they keep their name and
//! can be dragged back. An LM Studio `publisher/repo` folder goes whole once
//! no model file would remain in it; `.internal/` is outside the models
//! folder and never touched. Your folders get no folder cleanup, but a note
//! when the file is inside a git repository or other symlinks point at it.
//! The operations themselves re-check what they act on, so a path that
//! changed kind since planning is refused rather than removed.

use super::{Action, Collateral, Draft, NoteCode, Removal, Target};
use modeld_providers::{ProviderRoot, lmstudio};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Model file extensions that keep an LM Studio folder alive.
const MODEL_EXTENSIONS: [&str; 2] = ["gguf", "safetensors"];

/// Plans trashing LM Studio files, or their whole folder once emptied.
pub(super) fn plan_lmstudio(targets: Option<&Vec<Target>>, draft: &mut Draft) {
    let Some(targets) = targets else { return };
    let mut by_folder: BTreeMap<Option<PathBuf>, Vec<&Target>> = BTreeMap::new();
    for target in targets {
        let folder = lmstudio::model_folder(&target.root.root, &target.reference.path);
        by_folder.entry(folder).or_default().push(target);
    }
    for (folder, targets) in by_folder {
        let paths: Vec<&Path> = targets
            .iter()
            .map(|target| target.reference.path.as_path())
            .collect();
        let Some(folder) = folder.filter(|folder| !holds_other_models(folder, &paths)) else {
            for target in targets {
                trash_file(target, draft);
            }
            continue;
        };
        let label = folder
            .strip_prefix(&targets[0].root.root)
            .unwrap_or(&folder)
            .display()
            .to_string();
        for (path, size) in files_in(&folder) {
            if !paths.contains(&path.as_path()) {
                draft.collateral.push((
                    Collateral {
                        path,
                        size,
                        reason: format!("rest of the LM Studio folder {label}"),
                    },
                    Removal::Trash,
                ));
            }
        }
        for target in &targets {
            draft
                .removals
                .insert(target.reference.path.clone(), Removal::Trash);
        }
        draft.step(targets[0], Action::TrashFolder, &folder);
    }
}

/// Plans trashing files in your folders, with git and symlink notes.
pub(super) fn plan_manual(
    targets: Option<&Vec<Target>>,
    roots: &[ProviderRoot],
    draft: &mut Draft,
) {
    let Some(targets) = targets else { return };
    let links = symlinks_to(targets, roots);
    for target in targets {
        let path = &target.reference.path;
        trash_file(target, draft);
        if let Some(repository) = git_repository(path) {
            draft.note(
                NoteCode::GitRepository,
                path,
                format!(
                    "inside the git repository {}; if git tracks this file, it shows as deleted",
                    repository.display()
                ),
                vec![repository],
            );
        }
        if let Some(links) = links.get(path).filter(|links| !links.is_empty()) {
            draft.note(
                NoteCode::Symlinks,
                path,
                format!(
                    "{} symlink(s) point at it and will stop working",
                    links.len()
                ),
                links.clone(),
            );
        }
    }
}

fn trash_file(target: &Target, draft: &mut Draft) {
    let path = &target.reference.path;
    draft.step(target, Action::Trash, path);
    draft.removals.insert(path.clone(), Removal::Trash);
}

/// Whether `folder` holds a model file other than `removing`.
fn holds_other_models(folder: &Path, removing: &[&Path]) -> bool {
    files_in(folder).iter().any(|(path, _)| {
        !removing.contains(&path.as_path())
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| MODEL_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
    })
}

/// Every file under `folder`, with its size.
fn files_in(folder: &Path) -> Vec<(PathBuf, u64)> {
    walkdir::WalkDir::new(folder)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_type().is_dir())
        .filter_map(|entry| {
            let size = entry.metadata().ok()?.len();
            Some((entry.into_path(), size))
        })
        .collect()
}

/// The nearest folder above `path` with a `.git` entry.
fn git_repository(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .find(|folder| folder.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Symlinks in scanned plain folders that resolve to each target.
fn symlinks_to(targets: &[Target], roots: &[ProviderRoot]) -> HashMap<PathBuf, Vec<PathBuf>> {
    let wanted: HashMap<PathBuf, PathBuf> = targets
        .iter()
        .filter_map(|target| {
            let path = &target.reference.path;
            Some((path.canonicalize().ok()?, path.clone()))
        })
        .collect();
    let mut found: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    let plain = roots.iter().filter(|root| {
        matches!(
            root.kind,
            modeld_core::ProviderKind::Manual | modeld_core::ProviderKind::LmStudio
        )
    });
    for root in plain {
        let links = walkdir::WalkDir::new(&root.root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(walkdir::DirEntry::path_is_symlink);
        for link in links {
            let target = link
                .path()
                .canonicalize()
                .ok()
                .and_then(|resolved| wanted.get(&resolved));
            if let Some(target) = target {
                found
                    .entry(target.clone())
                    .or_default()
                    .push(link.into_path());
            }
        }
    }
    for links in found.values_mut() {
        links.sort();
        links.dedup();
    }
    found
}

/// Moves `path` to the Trash.
///
/// # Errors
/// `trash` failed, or the path is still there.
pub fn trash(path: &Path) -> Result<(), String> {
    let output = Command::new("/usr/bin/trash")
        .arg("-s")
        .arg(path)
        .output()
        .map_err(|error| format!("cannot run trash ({error})"))?;
    if !output.status.success() {
        return Err(format!(
            "could not move {} to the Trash: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(format!("{} is still there after trashing", path.display()));
    }
    Ok(())
}

/// Deletes a regular file; anything else is refused.
///
/// # Errors
/// The path is no longer a regular file, or deleting it failed.
pub(super) fn delete_file(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot check {} ({error})", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is no longer a regular file", path.display()));
    }
    std::fs::remove_file(path)
        .map_err(|error| format!("cannot delete {} ({error})", path.display()))
}

/// Deletes a symlink; anything else is refused.
///
/// # Errors
/// The path is no longer a symlink, or unlinking it failed.
pub(super) fn unlink(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot check {} ({error})", path.display()))?;
    if !metadata.file_type().is_symlink() {
        return Err(format!("{} is no longer a symlink", path.display()));
    }
    std::fs::remove_file(path)
        .map_err(|error| format!("cannot unlink {} ({error})", path.display()))
}

/// Deletes a folder and everything in it; a symlink is refused.
///
/// # Errors
/// The path is no longer a real folder, or deleting it failed.
pub(super) fn delete_folder(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot check {} ({error})", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!("{} is no longer a folder", path.display()));
    }
    std::fs::remove_dir_all(path)
        .map_err(|error| format!("cannot delete {} ({error})", path.display()))
}
