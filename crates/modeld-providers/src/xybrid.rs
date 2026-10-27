//! Xybrid's local model cache, rooted at `~/.xybrid`.
//!
//! Model files include extracted GGUF, safetensors, ONNX, and opaque `.xyb`
//! bundles. Runtime libraries, framework downloads, and traces are not models.

use crate::ProviderRoot;
use modeld_core::ProviderKind;
use std::path::{Path, PathBuf};

/// Detects Xybrid without creating its cache.
#[must_use]
pub fn detect(home: &Path) -> Option<ProviderRoot> {
    let root = home.join(".xybrid");
    root.is_dir().then(|| ProviderRoot {
        kind: ProviderKind::Xybrid,
        excluded: excluded(home),
        root,
        label_prefix: None,
    })
}

pub(crate) fn excluded(home: &Path) -> Vec<PathBuf> {
    let root = home.join(".xybrid");
    [
        "traces",
        "cache/precompiled",
        "cache/xcframework",
        "cache/ort-ios",
    ]
    .into_iter()
    .map(|relative| root.join(relative))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use modeld_core::Format;

    #[test]
    fn absent_cache_is_not_created() {
        let home = tempfile::tempdir().expect("home");
        assert!(detect(home.path()).is_none());
        assert!(!home.path().join(".xybrid").exists());
    }

    #[test]
    fn detects_models_and_leaves_runtime_caches_and_sidecars_alone() {
        let home = tempfile::tempdir().expect("home");
        let root = home.path().join(".xybrid");
        for relative in [
            "cache/models/SmolLM/universal.xyb",
            "cache/extracted/llm/model.safetensors",
            "cache/extracted/embed/model.onnx",
            "cache/hf/llm/model.gguf",
            "cache/hf/llm/model_metadata.json",
            "cache/models/SmolLM/universal.xyb.sha256",
            "cache/precompiled/compiler.bin",
            "cache/xcframework/test/model.gguf",
            "cache/ort-ios/test/model.onnx",
            "traces/test.gguf",
        ] {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, b"model file content").expect("write");
        }
        let detected = detect(home.path()).expect("Xybrid");
        assert_eq!(detected.kind, ProviderKind::Xybrid);
        let outcome = crate::scan::scan(&[detected], 1);
        assert_eq!(outcome.artifacts.len(), 4);
        assert!(outcome.skipped.is_empty());
        assert!(
            outcome
                .artifacts
                .iter()
                .all(|model| { model.provider == ProviderKind::Xybrid && model.digest.is_none() })
        );
        assert!(
            outcome
                .artifacts
                .iter()
                .any(|model| { model.format == Some(Format::Other("XYB".to_string())) })
        );
    }
}
