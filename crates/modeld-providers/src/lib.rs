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
pub mod xybrid;

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
            xybrid::detect(&home),
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
            untrackable: false,
        }),
    }
    let protected: Vec<_> = huggingface::excluded(&home)
        .into_iter()
        .chain(lmstudio::excluded(&home))
        .chain(xybrid::excluded(&home))
        .collect();
    protect_manual_roots(&mut detection.roots, &protected);
    detection
}

/// A broad configured folder must not re-enter an app's protected subtrees.
///
/// Provider paths may use a home-folder alias while Manual roots are canonical.
/// Resolve existing parents as well as existing exclusions: a not-yet-created
/// runtime cache must stay excluded when it appears during a later scan.
fn protect_manual_roots(roots: &mut [ProviderRoot], extra: &[PathBuf]) {
    let protected: Vec<_> = roots
        .iter()
        .filter(|root| root.kind != ProviderKind::Manual)
        .flat_map(|root| &root.excluded)
        .chain(extra)
        .map(|path| canonical_prefix(path))
        .collect();
    for root in roots
        .iter_mut()
        .filter(|root| root.kind == ProviderKind::Manual)
    {
        root.excluded.extend(
            protected
                .iter()
                .filter(|path| path.starts_with(&root.root) || root.root.starts_with(path))
                .cloned(),
        );
        root.excluded.sort();
        root.excluded.dedup();
    }
}

fn canonical_prefix(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        if let Ok(canonical) = ancestor.canonicalize() {
            return canonical.join(path.strip_prefix(ancestor).unwrap_or(path));
        }
    }
    path.to_path_buf()
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

    #[test]
    fn broad_manual_roots_keep_app_exclusions_even_before_the_cache_exists() {
        let home = tempfile::tempdir().expect("home");
        let canonical_home = home.path().canonicalize().expect("canonical home");
        let hub = home.path().join(".cache/huggingface/hub");
        std::fs::create_dir_all(&hub).expect("hub");
        let lm_models = home.path().join(".lmstudio/models");
        std::fs::create_dir_all(&lm_models).expect("LM models");
        let xybrid = home.path().join(".xybrid");
        std::fs::create_dir_all(&xybrid).expect("Xybrid");
        let mut roots = vec![
            ProviderRoot {
                kind: ProviderKind::HuggingFace,
                root: hub,
                excluded: vec![home.path().join(".cache/huggingface/xet")],
                label_prefix: None,
            },
            lmstudio::detect(home.path()).expect("LM Studio"),
            xybrid::detect(home.path()).expect("Xybrid"),
            ProviderRoot {
                kind: ProviderKind::Manual,
                root: canonical_home.clone(),
                excluded: vec![canonical_home.join(".modeld")],
                label_prefix: None,
            },
        ];
        protect_manual_roots(&mut roots, &[]);
        // These caches did not exist when the exclusions were propagated.
        for relative in [
            ".cache/huggingface/xet/chunk.gguf",
            ".lmstudio/.internal/staging.gguf",
            ".lmstudio/hub/transfer.gguf",
            ".xybrid/cache/precompiled/runtime.bin",
            ".xybrid/cache/xcframework/test/model.gguf",
            ".xybrid/cache/ort-ios/test/model.onnx",
            ".xybrid/traces/trace.gguf",
            ".modeld/blobs/stored.gguf",
            "models/allowed.gguf",
        ] {
            let path = home.path().join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, b"model content").expect("write");
        }
        let outcome = scan::scan(&roots, 1);
        assert!(outcome.skipped.is_empty());
        assert_eq!(outcome.artifacts.len(), 1);
        assert_eq!(
            outcome.artifacts[0].path,
            canonical_home.join("models/allowed.gguf")
        );
    }
}
