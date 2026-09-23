//! Classifies synced artifacts and reads header facts for the registry.
//!
//! Runs once per newly stored artifact: decide whether the file is model
//! weights or a model-adjacent asset (tokenizer, vocabulary), then ask the
//! format parser what the header says. Everything recorded here is
//! display-only; identity remains the digest.

use modeld_core::{Artifact, Format};
use modeld_store::Semantics;
use std::path::Path;

/// Filenames that are model-adjacent assets, not weights.
///
/// Matched against the reference filename (HF snapshot name or path basename),
/// lowercased. These files list alongside models in provider caches but should
/// not read as models in `ls`.
const ASSET_FILENAMES: [&str; 10] = [
    "tokenizer.json",
    "tokenizer.model",
    "tokenizer_config.json",
    "vocab.json",
    "vocab.txt",
    "merges.txt",
    "special_tokens_map.json",
    "added_tokens.json",
    "spiece.model",
    "sentencepiece.bpe.model",
];

/// Builds the registry semantics for one artifact from its canonical blob.
///
/// An unreadable header is not fatal: the artifact keeps its kind, `warn`
/// hears why the facts are missing.
pub fn analyze(artifact: &Artifact, blob: &Path, warn: impl FnOnce(String)) -> Semantics {
    let kind = if is_asset(artifact) { "asset" } else { "model" };
    let info = inspect(artifact, blob, warn).unwrap_or_default();
    Semantics {
        kind: kind.to_string(),
        name: info.name,
        architecture: info.architecture,
        quant: info.quant,
        params: info.params,
    }
}

fn inspect(
    artifact: &Artifact,
    blob: &Path,
    warn: impl FnOnce(String),
) -> Option<modeld_formats::ModelInfo> {
    let parsed = match artifact.format {
        Some(Format::Gguf) => modeld_formats::inspect_gguf(blob),
        Some(Format::Safetensors) => modeld_formats::inspect_safetensors(blob),
        _ => return None,
    };
    match parsed {
        Ok(info) => Some(info),
        Err(error) => {
            warn(format!(
                "could not read header of {} ({error})",
                artifact.path.display()
            ));
            None
        }
    }
}

/// The filename users know the artifact by: HF label suffix or path basename.
///
/// HF blob paths are content-addressed hex, so the snapshot filename lives in
/// the label (`repo: tokenizer.json`); loose trees carry it in the path.
fn reference_filename(artifact: &Artifact) -> String {
    artifact
        .label
        .as_deref()
        .and_then(|label| label.rsplit_once(": ").map(|(_, filename)| filename))
        .or_else(|| artifact.path.file_name().and_then(|name| name.to_str()))
        .unwrap_or_default()
        .to_lowercase()
}

fn is_asset(artifact: &Artifact) -> bool {
    ASSET_FILENAMES.contains(&reference_filename(artifact).as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn artifact(path: &str, label: Option<&str>) -> Artifact {
        Artifact {
            path: PathBuf::from(path),
            size: 0,
            provider: modeld_core::ProviderKind::Manual,
            format: None,
            digest: None,
            digest_verified: false,
            label: label.map(str::to_string),
            file_id: None,
            link_count: None,
        }
    }

    #[test]
    fn hf_tokenizer_label_classifies_as_asset() {
        let artifact = artifact(
            "/hub/models--Qwen--X/blobs/5f9e4d49",
            Some("Qwen/Qwen3.5-0.8B: tokenizer.json"),
        );
        assert!(is_asset(&artifact));
    }

    #[test]
    fn loose_gguf_classifies_as_model() {
        let artifact = artifact("/fixtures/models/Bonsai-1.7B-Q1_0.gguf", None);
        assert!(!is_asset(&artifact));
    }

    #[test]
    fn hf_weights_label_classifies_as_model() {
        let artifact = artifact(
            "/hub/models--d--d/blobs/7c391983",
            Some("distilbert/distilbert: model.safetensors"),
        );
        assert!(!is_asset(&artifact));
    }

    #[test]
    fn bare_merges_txt_classifies_as_asset() {
        let artifact = artifact("/some/tree/merges.txt", None);
        assert!(is_asset(&artifact));
    }
}
