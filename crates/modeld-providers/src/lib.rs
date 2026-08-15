//! Provider adapters: where each tool keeps model bytes and how to read identity out
//! of its metadata. Ground truth for each provider lives in `providers/*.md` at the
//! repo root. User-configured extra roots are handled by [`config`].

use modeld_core::ProviderKind;
use std::path::PathBuf;

pub mod config;
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
    /// Prepended to artifact labels so files from different roots stay
    /// distinguishable (used by Manual roots; provider caches label well already).
    pub label_prefix: Option<String>,
}

/// Everything detection produced: scan roots plus non-fatal warnings.
#[derive(Debug, Default)]
pub struct Detection {
    pub roots: Vec<ProviderRoot>,
    pub warnings: Vec<String>,
}

/// Detects known providers plus configured extra roots on this machine.
///
/// Absent providers are omitted; a broken config file becomes a warning, never a
/// failure. Detection reads the filesystem and env vars, nothing else.
#[must_use]
pub fn detect_all() -> Detection {
    let home = home_dir();
    let mut detection = Detection::default();
    detection.roots.extend(
        [
            ollama::detect(&home),
            huggingface::detect(&home),
            lmstudio::detect(&home),
        ]
        .into_iter()
        .flatten(),
    );
    match config::load(&home.join(".modeld/config.toml")) {
        Ok(user_config) => detection
            .roots
            .extend(config::manual_roots(&user_config, &home)),
        Err(warning) => detection.warnings.push(warning),
    }
    detection
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}
