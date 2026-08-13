//! Provider adapters: where each tool keeps model bytes and how to read identity out
//! of its metadata. Ground truth for each provider lives in `providers/*.md` at the
//! repo root.

use modeld_core::ProviderKind;
use std::path::PathBuf;

pub mod huggingface;
pub mod lmstudio;
pub mod ollama;
pub mod scan;

/// A detected provider installation: the directory subtree modeld may scan.
#[derive(Debug, Clone)]
pub struct ProviderRoot {
    pub kind: ProviderKind,
    pub root: PathBuf,
    /// Subpaths scanning must never enter (transfer caches, staging areas,
    /// non-model state).
    pub excluded: Vec<PathBuf>,
}

/// Detect all known providers present on this machine. Absent providers are simply
/// omitted — detection reads the filesystem and env vars, nothing else.
#[must_use]
pub fn detect_all() -> Vec<ProviderRoot> {
    let home = home_dir();
    [
        ollama::detect(&home),
        huggingface::detect(&home),
        lmstudio::detect(&home),
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}
