//! Ollama: `~/.ollama/models` (override `OLLAMA_MODELS`). Blobs are `sha256-<64hex>`
//! files whose name is the claimed content digest; manifests are OCI image manifests
//! naming each blob layer. See providers/ollama.md.

use crate::ProviderRoot;
use crate::scan::{ScanOutcome, Skipped, artifact_from_file, detect_format};
use modeld_core::{Algorithm, Digest, ProviderKind};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn detect(home: &Path) -> Option<ProviderRoot> {
    let root = std::env::var_os("OLLAMA_MODELS")
        .map_or_else(|| home.join(".ollama/models"), PathBuf::from);
    root.is_dir().then(|| ProviderRoot {
        kind: ProviderKind::Ollama,
        root,
        excluded: vec![],
        label_prefix: None,
    })
}

/// Harvest the claimed digest from an Ollama blob filename (`sha256-<64hex>`).
/// Partial downloads (`sha256-*-partial*`) yield `None`.
#[must_use]
pub fn digest_from_blob_name(file_name: &str) -> Option<Digest> {
    let hex = file_name.strip_prefix("sha256-")?;
    if hex.len() != 64 {
        return None; // partial download or foreign naming
    }
    Digest::from_hex(Algorithm::Sha256, hex).ok()
}

/// Collects model artifacts from an Ollama root into `outcome`.
pub fn collect(root: &ProviderRoot, min_size: u64, outcome: &mut ScanOutcome) {
    let labels = labels_by_digest(&root.root.join("manifests"), outcome);
    let blobs = root.root.join("blobs");
    let entries = match std::fs::read_dir(&blobs) {
        Ok(entries) => entries,
        Err(error) => {
            outcome.skipped.push(Skipped {
                path: blobs,
                reason: format!("cannot list blobs: {error}"),
            });
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() || metadata.len() < min_size {
            continue;
        }
        let file_name = entry.file_name();
        let Some(digest) = file_name.to_str().and_then(digest_from_blob_name) else {
            outcome.skipped.push(Skipped {
                path,
                reason: "not a completed sha256 blob".to_string(),
            });
            continue;
        };
        let label = labels.get(&digest).cloned();
        let format = detect_format(&path);
        outcome.artifacts.push(artifact_from_file(
            path,
            &metadata,
            ProviderKind::Ollama,
            format,
            Some(digest),
            label,
        ));
    }
}

/// OCI image manifest, one JSON file per model tag (no file extension).
#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    layers: Vec<Layer>,
}

#[derive(Debug, Deserialize)]
struct Layer {
    digest: String,
}

/// Maps blob digests to `ollama/<model>:<tag>` labels by reading all manifests.
fn labels_by_digest(manifests: &Path, outcome: &mut ScanOutcome) -> HashMap<Digest, String> {
    let mut labels = HashMap::new();
    for entry in walkdir::WalkDir::new(manifests)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let Some(label) = model_tag_label(entry.path()) else {
            continue;
        };
        let Ok(contents) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<Manifest>(&contents) else {
            outcome.skipped.push(Skipped {
                path: entry.path().to_path_buf(),
                reason: "unparseable manifest".to_string(),
            });
            continue;
        };
        for layer in manifest.layers {
            if let Ok(digest) = layer.digest.parse::<Digest>() {
                labels.entry(digest).or_insert_with(|| label.clone());
            }
        }
    }
    labels
}

/// Renders `.../manifests/<registry>/<ns>/<model>/<tag>` as `ollama/<model>:<tag>`.
fn model_tag_label(manifest_path: &Path) -> Option<String> {
    let tag = manifest_path.file_name()?.to_str()?;
    let model = manifest_path.parent()?.file_name()?.to_str()?;
    Some(format!("ollama/{model}:{tag}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL_HEX: &str = "797b70c4edf85907fe0a49eb85811256f65fa0f7bf52166b147fd16be2be4662";

    #[test]
    fn harvests_digest_from_blob_name() {
        let digest = digest_from_blob_name(&format!("sha256-{MODEL_HEX}")).expect("valid name");
        assert_eq!(digest.to_string(), format!("sha256:{MODEL_HEX}"));
    }

    #[test]
    fn rejects_partial_and_foreign_names() {
        assert!(digest_from_blob_name("sha256-abc123-partial").is_none());
        assert!(digest_from_blob_name("model.gguf").is_none());
    }

    #[test]
    fn collects_blobs_with_manifest_labels() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let root = dir.path();
        let blob_dir = root.join("blobs");
        std::fs::create_dir_all(&blob_dir).expect("mkdir blobs");
        let mut blob_content = b"GGUF".to_vec();
        blob_content.resize(2048, 0);
        std::fs::write(blob_dir.join(format!("sha256-{MODEL_HEX}")), &blob_content)
            .expect("write blob");
        std::fs::write(blob_dir.join("sha256-feed-partial-0"), b"partial").expect("write partial");

        let manifest_dir = root.join("manifests/registry.ollama.ai/library/all-minilm");
        std::fs::create_dir_all(&manifest_dir).expect("mkdir manifests");
        let manifest = format!(
            r#"{{"schemaVersion":2,"layers":[{{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:{MODEL_HEX}","size":2048}}]}}"#
        );
        std::fs::write(manifest_dir.join("22m"), manifest).expect("write manifest");

        let provider = ProviderRoot {
            kind: ProviderKind::Ollama,
            root: root.to_path_buf(),
            excluded: vec![],
            label_prefix: None,
        };
        let mut outcome = ScanOutcome::new();
        collect(&provider, 1024, &mut outcome);

        assert_eq!(outcome.artifacts.len(), 1);
        let artifact = &outcome.artifacts[0];
        assert_eq!(artifact.label.as_deref(), Some("ollama/all-minilm:22m"));
        assert_eq!(artifact.format, Some(modeld_core::Format::Gguf));
        assert!(!artifact.digest_verified);
        assert_eq!(
            artifact.digest.as_ref().map(ToString::to_string),
            Some(format!("sha256:{MODEL_HEX}"))
        );
    }
}
