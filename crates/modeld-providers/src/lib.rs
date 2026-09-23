//! Provider adapters: where each tool keeps model bytes and how to read identity out
//! of its metadata. Ground truth for each provider lives in `providers/*.md` at the
//! repo root. User-configured extra roots are handled by [`config`].

use modeld_core::ProviderKind;
use scan::{ScanOutcome, Skipped};
use std::path::{Path, PathBuf};

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

/// Everything detection produced: scan roots plus sources it could not read.
#[derive(Debug, Default)]
pub struct Detection {
    pub roots: Vec<ProviderRoot>,
    /// Root sources that could not be read, such as a broken config file.
    ///
    /// The roots they would define are unknown, so every scan of this
    /// detection is incomplete: callers must not prune references on it.
    pub unreadable: Vec<Skipped>,
}

impl Detection {
    /// Scans every detected root; unreadable root sources count as skipped.
    #[must_use]
    pub fn scan(&self, min_size: u64) -> ScanOutcome {
        let mut outcome = scan::scan(&self.roots, min_size);
        outcome.skipped.extend(self.unreadable.iter().cloned());
        outcome
    }
}

/// Detects known providers plus configured extra roots on this machine.
///
/// Absent providers are omitted; a broken config file is recorded in
/// [`Detection::unreadable`], never a failure. Detection reads the filesystem
/// and env vars, nothing else.
#[must_use]
pub fn detect_all() -> Detection {
    detect_in(&home_dir())
}

fn detect_in(home: &Path) -> Detection {
    let home = home.to_path_buf();
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
    let config_path = home.join(".modeld/config.toml");
    match config::load(&config_path) {
        Ok(user_config) => detection
            .roots
            .extend(config::manual_roots(&user_config, &home)),
        Err(reason) => detection.unreadable.push(Skipped {
            path: config_path,
            reason,
        }),
    }
    detection
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_config_makes_every_scan_incomplete() {
        let home = tempfile::tempdir().expect("create temp dir");
        let config = home.path().join(".modeld/config.toml");
        std::fs::create_dir_all(config.parent().expect("parent")).expect("mkdir");
        std::fs::write(&config, "not [valid toml").expect("write config");

        let detection = detect_in(home.path());

        assert_eq!(detection.unreadable.len(), 1);
        assert_eq!(detection.unreadable[0].path, config);
        assert_eq!(detection.scan(1).skipped.len(), 1);
    }

    #[test]
    fn missing_config_leaves_detection_complete() {
        let home = tempfile::tempdir().expect("create temp dir");

        let detection = detect_in(home.path());

        assert!(detection.unreadable.is_empty());
        assert!(detection.scan(1).skipped.is_empty());
    }
}
