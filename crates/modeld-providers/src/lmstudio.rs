//! LM Studio: `~/.lmstudio/models/<publisher>/<repo>/<file>`. Plain files, no hashes
//! in any metadata — modeld computes digests itself. `.internal/` holds app state and
//! staging downloads; never scanned. See providers/lmstudio.md.

use crate::ProviderRoot;
use crate::scan::{ScanOutcome, collect_model_tree};
use modeld_core::ProviderKind;
use std::path::{Path, PathBuf};

#[must_use]
pub fn detect(home: &Path) -> Option<ProviderRoot> {
    let base = home.join(".lmstudio");
    let models = base.join("models");
    models.is_dir().then(|| ProviderRoot {
        kind: ProviderKind::LmStudio,
        root: models,
        excluded: vec![base.join(".internal"), base.join("hub")],
        label_prefix: None,
    })
}

/// The `<publisher>/<repo>` folder under `root` that holds `file`.
///
/// LM Studio lists one model per such folder; `None` when `file` is not at
/// least two folders deep under `root`.
#[must_use]
pub fn model_folder(root: &Path, file: &Path) -> Option<PathBuf> {
    let relative = file.strip_prefix(root).ok()?;
    let mut parts = relative.iter();
    let (publisher, repo) = (parts.next()?, parts.next()?);
    parts.next()?;
    Some(root.join(publisher).join(repo))
}

/// Collects model files from the LM Studio models tree.
pub fn collect(root: &ProviderRoot, min_size: u64, outcome: &mut ScanOutcome) {
    collect_model_tree(root, min_size, outcome);
}

#[cfg(test)]
mod tests {
    use super::*;
    use modeld_core::Format;

    #[test]
    fn model_folder_is_publisher_and_repo() {
        let root = Path::new("/Users/me/.lmstudio/models");
        let file = root.join("lmstudio-community/Qwen3-GGUF/Qwen3-Q4_K_M.gguf");

        assert_eq!(
            model_folder(root, &file),
            Some(root.join("lmstudio-community/Qwen3-GGUF"))
        );
        assert_eq!(model_folder(root, &root.join("loose.gguf")), None);
        assert_eq!(model_folder(root, Path::new("/elsewhere/a/b/c.gguf")), None);
    }

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
            label_prefix: None,
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
