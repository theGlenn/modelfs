//! LM Studio: `~/.lmstudio/models/<publisher>/<repo>/<file>`. Plain files, no hashes
//! in any metadata — modeld computes digests itself. `.internal/` holds app state and
//! staging downloads; never scanned. See providers/lmstudio.md.

use crate::ProviderRoot;
use crate::scan::{ScanOutcome, artifact_from_file, detect_format};
use modeld_core::ProviderKind;
use std::path::Path;

#[must_use]
pub fn detect(home: &Path) -> Option<ProviderRoot> {
    let base = home.join(".lmstudio");
    let models = base.join("models");
    models.is_dir().then(|| ProviderRoot {
        kind: ProviderKind::LmStudio,
        root: models,
        excluded: vec![base.join(".internal"), base.join("hub")],
    })
}

/// Collects model files from the LM Studio models tree.
///
/// Only files with a recognized model format are included: LM Studio trees also hold
/// sidecar configs and app droppings that are not model weights. Symlinks are skipped
/// — a symlinked model file already shares storage with its target.
pub fn collect(root: &ProviderRoot, min_size: u64, outcome: &mut ScanOutcome) {
    for entry in walkdir::WalkDir::new(&root.root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !root.excluded.iter().any(|ex| e.path().starts_with(ex)))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() < min_size {
            continue;
        }
        let path = entry.path().to_path_buf();
        let Some(format) = detect_format(&path) else {
            continue;
        };
        let label = path
            .strip_prefix(&root.root)
            .ok()
            .map(|rel| rel.display().to_string());
        outcome.artifacts.push(artifact_from_file(
            path,
            &metadata,
            ProviderKind::LmStudio,
            Some(format),
            None,
            label,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modeld_core::Format;

    #[test]
    fn collects_model_files_and_ignores_sidecars_and_small_files() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let repo = dir.path().join("lmstudio-community/gpt-oss-20b-GGUF");
        std::fs::create_dir_all(&repo).expect("mkdir repo");
        let mut gguf = b"GGUF".to_vec();
        gguf.resize(2048, 1);
        std::fs::write(repo.join("gpt-oss-20b-MXFP4.gguf"), &gguf).expect("write gguf");
        std::fs::write(repo.join("config.json"), vec![b'{'; 2048]).expect("write sidecar");
        std::fs::write(repo.join("tiny.gguf"), b"GGUF").expect("write tiny");

        let provider = ProviderRoot {
            kind: ProviderKind::LmStudio,
            root: dir.path().to_path_buf(),
            excluded: vec![],
        };
        let mut outcome = ScanOutcome::new();
        collect(&provider, 1024, &mut outcome);

        assert_eq!(outcome.artifacts.len(), 1);
        let artifact = &outcome.artifacts[0];
        assert_eq!(artifact.format, Some(Format::Gguf));
        assert_eq!(
            artifact.label.as_deref(),
            Some("lmstudio-community/gpt-oss-20b-GGUF/gpt-oss-20b-MXFP4.gguf")
        );
        assert!(artifact.digest.is_none());
    }
}
