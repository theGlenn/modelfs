//! The core inventory unit: one file that stores model bytes.

use crate::digest::Digest;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ProviderKind {
    Ollama,
    /// Classic HF hub cache (`models--*/blobs`). The Xet chunk cache is deliberately
    /// NOT a provider — it is excluded from scanning entirely (see providers/xet.md).
    HuggingFace,
    LmStudio,
    /// User-managed loose files (e.g. `~/models/*.gguf`) found via configured extra roots.
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Format {
    Gguf,
    Safetensors,
    Onnx,
    Other(String),
}

/// Identifies a file's storage identity: `(device, inode)`.
///
/// Two paths with equal `FileId` are hardlinks to the same inode and already share
/// storage — they must count once when estimating reclaimable space. APFS clones have
/// *distinct* inodes and are not detectable this way; clone-aware accounting comes
/// with the M3 registry, which records what modeld itself cloned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId {
    pub device: u64,
    pub inode: u64,
}

/// A scanned model artifact.
///
/// `digest` is `None` until harvested from provider metadata or computed.
/// `digest_verified` is true only when modeld itself hashed the file at `path` —
/// a digest harvested from a blob filename or manifest is a claim, not a verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub path: PathBuf,
    pub size: u64,
    pub provider: ProviderKind,
    pub format: Option<Format>,
    pub digest: Option<Digest>,
    pub digest_verified: bool,
    /// Human-readable origin, e.g. `ollama/all-minilm:22m` or
    /// `Qwen/Qwen3.5-0.8B: model.safetensors`. Display-only; never an identity.
    pub label: Option<String>,
    pub file_id: Option<FileId>,
}
