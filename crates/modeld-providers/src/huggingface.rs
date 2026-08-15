//! Hugging Face hub cache: `~/.cache/huggingface/hub` (overrides `HF_HUB_CACHE`,
//! `HF_HOME`). LFS blobs are named by their sha256; small git files by git-SHA-1.
//! The sibling `xet/` tree is a chunk transfer cache and is never scanned.
//! See providers/huggingface.md and providers/xet.md.

use crate::ProviderRoot;
use crate::scan::{ScanOutcome, artifact_from_file, detect_format};
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
    let Ok(entries) = std::fs::read_dir(&root.root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir_name = entry.file_name();
        let Some(repo) = dir_name.to_str().and_then(repo_id_from_cache_dir) else {
            continue;
        };
        collect_repo(&entry.path(), &repo, min_size, outcome);
    }
}

/// Renders a cache dir name like `models--Qwen--Qwen3.5-0.8B` as `Qwen/Qwen3.5-0.8B`.
fn repo_id_from_cache_dir(dir_name: &str) -> Option<String> {
    let encoded = dir_name.strip_prefix("models--")?;
    Some(encoded.replace("--", "/"))
}

fn collect_repo(repo_dir: &Path, repo: &str, min_size: u64, outcome: &mut ScanOutcome) {
    let filenames = filenames_by_blob(&repo_dir.join("snapshots"));
    let Ok(entries) = std::fs::read_dir(repo_dir.join("blobs")) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
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
