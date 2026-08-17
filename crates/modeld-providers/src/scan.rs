//! Scan orchestration: collect artifacts from provider roots, hash where needed.
//!
//! Scanning is read-only and non-fatal by construction: unreadable files become
//! [`Skipped`] entries, never errors or panics — a scan of a live machine must
//! survive files being downloaded, moved, or deleted mid-walk.

use crate::{ProviderRoot, huggingface, lmstudio, ollama};
use modeld_core::{Artifact, Digest, FileId, Format, ProviderKind, dedup};
use std::fs::Metadata;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Files smaller than this are never model weights worth tracking.
///
/// Large enough to skip config/tokenizer JSON noise, small enough to keep tiny
/// embedding models (smallest seen in the wild: ~20 MB GGUF).
pub const DEFAULT_MIN_SIZE: u64 = 1024 * 1024;

/// A file the scan looked at but could not or should not include.
#[derive(Debug, Clone)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: String,
}

/// Everything a scan produced: artifacts plus non-fatal skips.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    pub artifacts: Vec<Artifact>,
    pub skipped: Vec<Skipped>,
}

impl ScanOutcome {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Collects artifacts from every detected provider root.
#[must_use]
pub fn scan(roots: &[ProviderRoot], min_size: u64) -> ScanOutcome {
    let mut outcome = ScanOutcome::new();
    for root in roots {
        match root.kind {
            ProviderKind::Ollama => ollama::collect(root, min_size, &mut outcome),
            ProviderKind::HuggingFace => huggingface::collect(root, min_size, &mut outcome),
            ProviderKind::LmStudio => lmstudio::collect(root, min_size, &mut outcome),
            ProviderKind::Manual => collect_model_tree(root, min_size, &mut outcome),
            _ => {}
        }
    }
    outcome
}

/// Hashes artifacts that need a digest for duplicate detection, in place.
///
/// Only artifacts selected by [`dedup::indices_needing_digest`] are read, so cost is
/// proportional to potential duplicates, not to total model storage. `progress` is
/// called with each path before it is hashed. Unreadable files are recorded as
/// skipped and left digestless.
pub fn hash_for_dedup(outcome: &mut ScanOutcome, mut progress: impl FnMut(&Path, u64)) {
    for index in dedup::indices_needing_digest(&outcome.artifacts) {
        let artifact = &mut outcome.artifacts[index];
        progress(&artifact.path, artifact.size);
        match Digest::sha256_file(&artifact.path) {
            Ok(digest) => {
                artifact.digest = Some(digest);
                artifact.digest_verified = true;
            }
            Err(error) => outcome.skipped.push(Skipped {
                path: artifact.path.clone(),
                reason: format!("hash failed: {error}"),
            }),
        }
    }
}

/// Collects recognized model files from a plain directory tree.
///
/// Shared by LM Studio and Manual (configured) roots: format-filtered walk, no
/// digests (computed on demand later), labels made from the root-relative path
/// prefixed with the root's `label_prefix` when set. Symlinks are skipped — a
/// symlinked model file already shares storage with its target.
pub fn collect_model_tree(root: &crate::ProviderRoot, min_size: u64, outcome: &mut ScanOutcome) {
    let entries = walkdir::WalkDir::new(&root.root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !root.excluded.iter().any(|ex| e.path().starts_with(ex)));
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                outcome.skipped.push(Skipped {
                    path: error
                        .path()
                        .map_or_else(|| root.root.clone(), Path::to_path_buf),
                    reason: format!("walk failed: {error}"),
                });
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                outcome.skipped.push(Skipped {
                    path: entry.path().to_path_buf(),
                    reason: format!("cannot stat: {error}"),
                });
                continue;
            }
        };
        if metadata.len() < min_size {
            continue;
        }
        let path = entry.path().to_path_buf();
        let Some(format) = detect_format(&path) else {
            continue;
        };
        let label = path.strip_prefix(&root.root).ok().map(|rel| {
            root.label_prefix.as_deref().map_or_else(
                || rel.display().to_string(),
                |prefix| format!("{prefix}/{}", rel.display()),
            )
        });
        outcome.artifacts.push(artifact_from_file(
            path,
            &metadata,
            root.kind,
            Some(format),
            None,
            label,
        ));
    }
}

/// Builds an artifact from an on-disk file, harvesting what stat can provide.
pub(crate) fn artifact_from_file(
    path: PathBuf,
    metadata: &Metadata,
    provider: ProviderKind,
    format: Option<Format>,
    digest: Option<Digest>,
    label: Option<String>,
) -> Artifact {
    Artifact {
        size: metadata.len(),
        file_id: Some(FileId {
            device: metadata.dev(),
            inode: metadata.ino(),
        }),
        link_count: Some(metadata.nlink()),
        path,
        provider,
        format,
        digest,
        digest_verified: false,
        label,
    }
}

/// Detects a model file format from its extension, falling back to magic bytes.
///
/// The magic-byte fallback matters for extensionless files (Ollama blobs, HF blobs).
pub(crate) fn detect_format(path: &Path) -> Option<Format> {
    let by_extension = path.extension().and_then(|ext| match ext.to_str()? {
        "gguf" => Some(Format::Gguf),
        "safetensors" => Some(Format::Safetensors),
        "onnx" => Some(Format::Onnx),
        "bin" | "pt" => Some(Format::Other("pytorch".to_string())),
        _ => None,
    });
    if by_extension.is_some() {
        return by_extension;
    }
    detect_format_by_magic(path)
}

fn detect_format_by_magic(path: &Path) -> Option<Format> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).ok()?;
    // GGUF files start with the ASCII magic "GGUF" (little-endian u32 0x46554747).
    if &magic == b"GGUF" {
        return Some(Format::Gguf);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_gguf_by_magic_without_extension() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("sha256-blob-without-extension");
        let mut file = std::fs::File::create(&path).expect("create file");
        file.write_all(b"GGUF\x03\x00\x00\x00rest").expect("write");
        assert_eq!(detect_format(&path), Some(Format::Gguf));
    }

    #[test]
    fn detects_safetensors_by_extension() {
        assert_eq!(
            detect_format(Path::new("/x/model.safetensors")),
            Some(Format::Safetensors)
        );
    }

    #[test]
    fn hash_for_dedup_only_hashes_size_collisions() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let make = |name: &str, content: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, content).expect("write fixture");
            let metadata = std::fs::metadata(&path).expect("stat fixture");
            artifact_from_file(path, &metadata, ProviderKind::Manual, None, None, None)
        };
        let mut outcome = ScanOutcome::new();
        outcome.artifacts = vec![
            make("a.gguf", b"same-size"),
            make("b.gguf", b"same-size"),
            make("c.gguf", b"unique-length-content"),
        ];

        let mut hashed = Vec::new();
        hash_for_dedup(&mut outcome, |path, _| hashed.push(path.to_path_buf()));

        assert_eq!(hashed.len(), 2);
        assert!(outcome.artifacts[0].digest_verified);
        assert!(outcome.artifacts[1].digest_verified);
        assert!(outcome.artifacts[2].digest.is_none());
        assert_eq!(outcome.artifacts[0].digest, outcome.artifacts[1].digest);
    }
}
