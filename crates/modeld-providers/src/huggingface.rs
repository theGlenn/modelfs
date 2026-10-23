//! Hugging Face hub cache: `~/.cache/huggingface/hub` (overrides `HF_HUB_CACHE`,
//! `HF_HOME`). LFS blobs are named by their sha256; small git files by git-SHA-1.
//! The sibling `xet/` tree is a chunk transfer cache and is never scanned.
//! See providers/huggingface.md and providers/xet.md.

use crate::ProviderRoot;
use crate::scan::{ScanOutcome, Skipped, artifact_from_file, detect_format};
use modeld_core::{Algorithm, Digest, ProviderKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn detect(home: &Path) -> Option<ProviderRoot> {
    let hf_home =
        std::env::var_os("HF_HOME").map_or_else(|| home.join(".cache/huggingface"), PathBuf::from);
    let hub = std::env::var_os("HF_HUB_CACHE").map_or_else(|| hf_home.join("hub"), PathBuf::from);
    hub.is_dir().then(|| ProviderRoot {
        kind: ProviderKind::HuggingFace,
        // Root is the hub cache itself; the xet tree lives outside it under HF_HOME
        // but is listed excluded defensively in case of layout drift.
        root: hub,
        excluded: vec![hf_home.join("xet")],
        label_prefix: None,
    })
}

/// Harvest the claimed digest from an HF blob filename. 64-hex names are LFS sha256
/// etags; 40-hex names are git blob SHA-1 (recorded, never trusted for dedupe).
/// `*.incomplete` files yield `None`.
#[must_use]
pub fn digest_from_blob_name(file_name: &str) -> Option<Digest> {
    if !file_name.chars().all(|c| c.is_ascii_hexdigit()) {
        return None; // covers *.incomplete and anything non-etag
    }
    match file_name.len() {
        64 => Digest::from_hex(Algorithm::Sha256, file_name).ok(),
        40 => Digest::from_hex(Algorithm::GitSha1, file_name).ok(),
        _ => None,
    }
}

/// Collects model artifacts from every `models--*` repo in the hub cache.
///
/// Only `blobs/` files become artifacts; `snapshots/` symlinks are read solely to
/// label blobs with their repo-relative filename. Dataset and space caches are
/// ignored — modeld tracks model artifacts.
pub fn collect(root: &ProviderRoot, min_size: u64, outcome: &mut ScanOutcome) {
    let entries = match std::fs::read_dir(&root.root) {
        Ok(entries) => entries,
        Err(error) => {
            outcome.skipped.push(Skipped {
                path: root.root.clone(),
                reason: format!("cannot list cache root: {error}"),
                untrackable: false,
            });
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                outcome.skipped.push(Skipped {
                    path: root.root.clone(),
                    reason: format!("cannot read directory entry: {error}"),
                    untrackable: false,
                });
                continue;
            }
        };
        let dir_name = entry.file_name();
        let Some(repo) = dir_name.to_str().and_then(repo_id_from_cache_dir) else {
            continue;
        };
        collect_repo(&entry.path(), &repo, min_size, outcome);
    }
}

/// The `org/name` repo a hub-cache blob belongs to.
///
/// Reads it from the path, `<hub>/models--<org>--<name>/blobs/<etag>`;
/// `None` for any other layout.
#[must_use]
pub fn repo_of_blob(blob: &Path) -> Option<String> {
    let blobs = blob.parent()?;
    if blobs.file_name()? != "blobs" {
        return None;
    }
    repo_id_from_cache_dir(blobs.parent()?.file_name()?.to_str()?)
}

/// Every snapshot symlink in the blob's repo that resolves to `blob`.
///
/// Links may sit in subfolders of a snapshot (`snapshots/<commit>/onnx/...`).
/// Comparing resolved paths makes `../../blobs/<etag>` and absolute targets
/// match alike. A missing blob has no links.
#[must_use]
pub fn links_to(blob: &Path) -> Vec<PathBuf> {
    let Some(repo) = blob.parent().and_then(Path::parent) else {
        return Vec::new();
    };
    let Ok(target) = blob.canonicalize() else {
        return Vec::new();
    };
    let mut links: Vec<PathBuf> = walkdir::WalkDir::new(repo.join("snapshots"))
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(walkdir::DirEntry::path_is_symlink)
        .filter(|entry| {
            entry
                .path()
                .canonicalize()
                .is_ok_and(|resolved| resolved == target)
        })
        .map(walkdir::DirEntry::into_path)
        .collect();
    links.sort();
    links
}

/// Files in a repo's `blobs/` other than `removing`, with their sizes.
///
/// In-flight downloads (`*.incomplete`) are included, so a caller deciding
/// whether the repo folder may go sees them.
#[must_use]
pub fn leftovers(repo: &Path, removing: &[PathBuf]) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(repo.join("blobs")) else {
        return Vec::new();
    };
    let mut left: Vec<(PathBuf, u64)> = entries
        .filter_map(Result::ok)
        .filter(|entry| !removing.contains(&entry.path()))
        .filter_map(|entry| Some((entry.path(), entry.metadata().ok()?.len())))
        .collect();
    left.sort();
    left
}

/// The download lock `huggingface_hub` holds while writing `blob`.
///
/// `<hub>/.locks/models--<org>--<name>/<etag>.lock`; `None` for any other
/// layout.
#[must_use]
pub fn lock_file(blob: &Path) -> Option<PathBuf> {
    let etag = blob.file_name()?.to_str()?;
    let repo = blob.parent()?.parent()?;
    let hub = repo.parent()?;
    Some(
        hub.join(".locks")
            .join(repo.file_name()?)
            .join(format!("{etag}.lock")),
    )
}

/// Renders a cache dir name like `models--Qwen--Qwen3.5-0.8B` as `Qwen/Qwen3.5-0.8B`.
fn repo_id_from_cache_dir(dir_name: &str) -> Option<String> {
    let encoded = dir_name.strip_prefix("models--")?;
    Some(encoded.replace("--", "/"))
}

fn collect_repo(repo_dir: &Path, repo: &str, min_size: u64, outcome: &mut ScanOutcome) {
    let filenames = filenames_by_blob(&repo_dir.join("snapshots"));
    let blobs = repo_dir.join("blobs");
    let entries = match std::fs::read_dir(&blobs) {
        Ok(entries) => entries,
        // A repo cached for metadata only (refs/.no_exist, no blobs/) is normal,
        // not a scan failure — it must not block reference pruning.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            outcome.skipped.push(Skipped {
                path: blobs,
                reason: format!("cannot list blobs: {error}"),
                untrackable: false,
            });
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                outcome.skipped.push(Skipped {
                    path: repo_dir.to_path_buf(),
                    reason: format!("cannot read directory entry: {error}"),
                    untrackable: false,
                });
                continue;
            }
        };
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                outcome.skipped.push(Skipped {
                    path: entry.path(),
                    reason: format!("cannot stat: {error}"),
                    untrackable: false,
                });
                continue;
            }
        };
        if !metadata.is_file() || metadata.len() < min_size {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        // `.incomplete` files are in-flight downloads; anything non-etag is foreign.
        let Some(digest) = digest_from_blob_name(name) else {
            continue;
        };
        let path = entry.path();
        let label = filenames
            .get(name)
            .map(|filename| format!("{repo}: {filename}"));
        let format = detect_format(&path);
        outcome.artifacts.push(artifact_from_file(
            path,
            &metadata,
            ProviderKind::HuggingFace,
            format,
            Some(digest),
            label,
        ));
    }
}

/// Maps blob filenames to repo-relative filenames by reading snapshot symlinks.
fn filenames_by_blob(snapshots: &Path) -> HashMap<String, String> {
    let mut filenames = HashMap::new();
    for entry in walkdir::WalkDir::new(snapshots)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(walkdir::DirEntry::path_is_symlink)
    {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let Some(blob_name) = target.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(filename) = entry.file_name().to_str() else {
            continue;
        };
        filenames
            .entry(blob_name.to_string())
            .or_insert_with(|| filename.to_string());
    }
    filenames
}

#[cfg(test)]
mod tests {
    use super::*;

    const LFS_HEX: &str = "04b1c301231dd422b8860db31311ab2721511346a32cb1e079c4c4e5f1fe4696";
    const GIT_HEX: &str = "9cb811ded68c6b737595de4a89886369351840f8";

    #[test]
    fn classifies_blob_names() {
        assert_eq!(
            digest_from_blob_name(LFS_HEX).expect("valid").algorithm(),
            Algorithm::Sha256
        );
        assert_eq!(
            digest_from_blob_name(GIT_HEX).expect("valid").algorithm(),
            Algorithm::GitSha1
        );
        assert!(digest_from_blob_name(&format!("{LFS_HEX}.incomplete")).is_none());
    }

    #[test]
    fn decodes_repo_id_from_cache_dir_name() {
        assert_eq!(
            repo_id_from_cache_dir("models--Qwen--Qwen3.5-0.8B").as_deref(),
            Some("Qwen/Qwen3.5-0.8B")
        );
        assert!(repo_id_from_cache_dir("datasets--meshllm--catalog").is_none());
    }

    #[test]
    fn blob_path_names_its_repo() {
        let blob = Path::new("/hub/models--Qwen--Qwen3.5-0.8B/blobs").join(LFS_HEX);

        assert_eq!(repo_of_blob(&blob).as_deref(), Some("Qwen/Qwen3.5-0.8B"));
        assert_eq!(
            repo_of_blob(Path::new("/hub/models--Qwen--X/snapshots/abc/model")),
            None
        );
        assert_eq!(repo_of_blob(Path::new("/elsewhere/blobs/abc")), None);
    }

    #[test]
    fn links_to_finds_every_snapshot_link_of_a_blob() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let repo = dir.path().join("models--Qwen--Tiny");
        std::fs::create_dir_all(repo.join("blobs")).expect("mkdir blobs");
        let blob = repo.join("blobs").join(LFS_HEX);
        std::fs::write(&blob, b"weights").expect("write blob");
        std::fs::write(repo.join("blobs").join(GIT_HEX), b"{}").expect("write config");
        let first = repo.join("snapshots/aaa");
        let second = repo.join("snapshots/bbb/onnx");
        std::fs::create_dir_all(&first).expect("mkdir");
        std::fs::create_dir_all(&second).expect("mkdir");
        std::os::unix::fs::symlink(
            format!("../../blobs/{LFS_HEX}"),
            first.join("model.safetensors"),
        )
        .expect("link");
        std::os::unix::fs::symlink(
            format!("../../../blobs/{LFS_HEX}"),
            second.join("model.onnx"),
        )
        .expect("link");
        std::os::unix::fs::symlink(format!("../../blobs/{GIT_HEX}"), first.join("config.json"))
            .expect("link");

        let links = links_to(&blob);

        assert_eq!(
            links,
            [first.join("model.safetensors"), second.join("model.onnx")]
        );
        assert_eq!(
            leftovers(&repo, &[blob]),
            [(repo.join("blobs").join(GIT_HEX), 2)]
        );
    }

    #[test]
    fn lock_file_sits_under_the_hub_locks_folder() {
        let blob = Path::new("/hub/models--Qwen--Tiny/blobs").join(LFS_HEX);

        assert_eq!(
            lock_file(&blob),
            Some(PathBuf::from(format!(
                "/hub/.locks/models--Qwen--Tiny/{LFS_HEX}.lock"
            )))
        );
    }

    #[test]
    fn collects_labeled_blobs_and_skips_incomplete() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let repo_dir = dir.path().join("models--Qwen--Tiny");
        let blob_dir = repo_dir.join("blobs");
        let snapshot_dir = repo_dir.join("snapshots/abc123");
        std::fs::create_dir_all(&blob_dir).expect("mkdir blobs");
        std::fs::create_dir_all(&snapshot_dir).expect("mkdir snapshot");

        std::fs::write(blob_dir.join(LFS_HEX), vec![7u8; 2048]).expect("write blob");
        std::fs::write(
            blob_dir.join(format!("{LFS_HEX}.incomplete")),
            vec![0u8; 4096],
        )
        .expect("write incomplete");
        std::os::unix::fs::symlink(
            format!("../../blobs/{LFS_HEX}"),
            snapshot_dir.join("model.safetensors"),
        )
        .expect("symlink");

        let provider = ProviderRoot {
            kind: ProviderKind::HuggingFace,
            root: dir.path().to_path_buf(),
            excluded: vec![],
            label_prefix: None,
        };
        let mut outcome = ScanOutcome::new();
        collect(&provider, 1024, &mut outcome);

        assert_eq!(outcome.artifacts.len(), 1);
        let artifact = &outcome.artifacts[0];
        assert_eq!(
            artifact.label.as_deref(),
            Some("Qwen/Tiny: model.safetensors")
        );
        assert_eq!(
            artifact.digest.as_ref().expect("digest").algorithm(),
            Algorithm::Sha256
        );
    }
}
