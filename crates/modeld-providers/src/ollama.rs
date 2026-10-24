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
                untrackable: false,
            });
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                outcome.skipped.push(Skipped {
                    path: blobs.clone(),
                    reason: format!("cannot read directory entry: {error}"),
                    untrackable: false,
                });
                continue;
            }
        };
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                outcome.skipped.push(Skipped {
                    path,
                    reason: format!("cannot stat: {error}"),
                    untrackable: false,
                });
                continue;
            }
        };
        if !metadata.is_file() || metadata.len() < min_size {
            continue;
        }
        let file_name = entry.file_name();
        let Some(digest) = file_name.to_str().and_then(digest_from_blob_name) else {
            outcome.skipped.push(Skipped {
                path,
                reason: "not a completed sha256 blob".to_string(),
                untrackable: true,
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

/// One Ollama model tag: its manifest file and the blobs it uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestInfo {
    /// The manifest file, `<root>/manifests/<registry>/<namespace>/<model>/<tag>`.
    pub path: PathBuf,
    /// The name Ollama's CLI and API use for the tag, such as `qwen3:4b`.
    pub api_name: String,
    /// Every layer digest the manifest lists, the model weights among them.
    pub digests: Vec<Digest>,
}

/// Every tag manifest under an Ollama models root, and what could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestScan {
    pub manifests: Vec<ManifestInfo>,
    /// Manifests or folders that could not be read or parsed. Any of them
    /// may name any blob, so no blob can be proven unused while this is
    /// not empty.
    pub unreadable: Vec<PathBuf>,
}

/// Reads every tag manifest under an Ollama models root.
///
/// Unreadable and unparseable manifests are left out, as a tag Ollama itself
/// cannot read names no blob; use [`manifest_scan`] to learn about them.
/// Tags sharing a blob each appear, so callers can list every tag using it.
#[must_use]
pub fn manifests(models_root: &Path) -> Vec<ManifestInfo> {
    manifest_scan(models_root).manifests
}

/// Reads every tag manifest, recording the ones (and folders) it could not.
#[must_use]
pub fn manifest_scan(models_root: &Path) -> ManifestScan {
    let manifests = models_root.join("manifests");
    let mut scan = ManifestScan::default();
    for entry in walkdir::WalkDir::new(&manifests).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let path = error
                    .path()
                    .map_or_else(|| manifests.clone(), Path::to_path_buf);
                // A models root without a manifests folder simply has no tags.
                if path != manifests {
                    scan.unreadable.push(path);
                }
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let parsed = layer_digests(entry.path()).ok().and_then(|digests| {
            let api_name = api_name(entry.path().strip_prefix(&manifests).ok()?)?;
            Some((api_name, digests))
        });
        match parsed {
            Some((api_name, digests)) => scan.manifests.push(ManifestInfo {
                path: entry.into_path(),
                api_name,
                digests,
            }),
            None => scan.unreadable.push(entry.into_path()),
        }
    }
    scan
}

/// Maps blob digests to `ollama/<model>:<tag>` labels by reading all manifests.
fn labels_by_digest(manifests: &Path, outcome: &mut ScanOutcome) -> HashMap<Digest, String> {
    let mut labels = HashMap::new();
    for entry in manifest_files(manifests) {
        let Some(label) = model_tag_label(entry.path()) else {
            continue;
        };
        let digests = match layer_digests(entry.path()) {
            Ok(digests) => digests,
            Err(ManifestError::Unreadable) => continue,
            Err(ManifestError::Unparseable) => {
                outcome.skipped.push(Skipped {
                    path: entry.path().to_path_buf(),
                    reason: "unparseable manifest".to_string(),
                    untrackable: true,
                });
                continue;
            }
        };
        for digest in digests {
            labels.entry(digest).or_insert_with(|| label.clone());
        }
    }
    labels
}

/// Why a manifest's layers could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestError {
    Unreadable,
    Unparseable,
}

/// Every regular file under the manifests directory, one per tag.
fn manifest_files(manifests: &Path) -> impl Iterator<Item = walkdir::DirEntry> {
    walkdir::WalkDir::new(manifests)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
}

/// The layer digests a manifest lists; malformed digests are left out.
fn layer_digests(manifest: &Path) -> Result<Vec<Digest>, ManifestError> {
    let contents = std::fs::read(manifest).map_err(|_unreadable| ManifestError::Unreadable)?;
    let manifest = serde_json::from_slice::<Manifest>(&contents)
        .map_err(|_unparseable| ManifestError::Unparseable)?;
    Ok(manifest
        .layers
        .into_iter()
        .filter_map(|layer| layer.digest.parse().ok())
        .collect())
}

/// Renders `.../manifests/<registry>/<ns>/<model>/<tag>` as `ollama/<model>:<tag>`.
fn model_tag_label(manifest_path: &Path) -> Option<String> {
    let tag = manifest_path.file_name()?.to_str()?;
    let model = manifest_path.parent()?.file_name()?.to_str()?;
    Some(format!("ollama/{model}:{tag}"))
}

/// The name Ollama shows for a manifest path relative to `manifests/`.
///
/// The default registry and the `library` namespace are implied, as in
/// `ollama list`: `registry.ollama.ai/library/qwen3/4b` is `qwen3:4b`, and
/// `hf.co/org/repo/Q4_K_M` stays `hf.co/org/repo:Q4_K_M`.
fn api_name(relative: &Path) -> Option<String> {
    let parts: Vec<&str> = relative
        .iter()
        .map(|part| part.to_str())
        .collect::<Option<_>>()?;
    let (tag, name) = parts.split_last()?;
    let name = match name {
        ["registry.ollama.ai", "library", model] => (*model).to_string(),
        ["registry.ollama.ai", namespace, model] => format!("{namespace}/{model}"),
        [] => return None,
        other => other.join("/"),
    };
    Some(format!("{name}:{tag}"))
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

    fn write_manifest(root: &Path, relative: &str, digests: &[&str]) {
        let path = root.join("manifests").join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir manifests");
        let layers: Vec<String> = digests
            .iter()
            .map(|hex| format!(r#"{{"digest":"sha256:{hex}","size":1}}"#))
            .collect();
        std::fs::write(
            path,
            format!(r#"{{"schemaVersion":2,"layers":[{}]}}"#, layers.join(",")),
        )
        .expect("write manifest");
    }

    #[test]
    fn every_tag_sharing_a_blob_is_listed() {
        let dir = tempfile::tempdir().expect("create temp dir");
        write_manifest(
            dir.path(),
            "registry.ollama.ai/library/qwen3/4b",
            &[MODEL_HEX],
        );
        write_manifest(
            dir.path(),
            "registry.ollama.ai/library/qwen3/latest",
            &[MODEL_HEX],
        );
        let digest: Digest = format!("sha256:{MODEL_HEX}").parse().expect("digest");

        let mut tags: Vec<String> = manifests(dir.path())
            .into_iter()
            .filter(|manifest| manifest.digests.contains(&digest))
            .map(|manifest| manifest.api_name)
            .collect();
        tags.sort();

        assert_eq!(tags, ["qwen3:4b", "qwen3:latest"]);
    }

    #[test]
    fn api_names_match_what_ollama_list_shows() {
        let name = |relative: &str| api_name(Path::new(relative));

        assert_eq!(
            name("registry.ollama.ai/library/all-minilm/22m").as_deref(),
            Some("all-minilm:22m")
        );
        assert_eq!(
            name("registry.ollama.ai/someone/model/q4").as_deref(),
            Some("someone/model:q4")
        );
        assert_eq!(
            name("hf.co/bartowski/Llama-3.2-1B-GGUF/Q4_K_M").as_deref(),
            Some("hf.co/bartowski/Llama-3.2-1B-GGUF:Q4_K_M")
        );
        assert_eq!(name("orphan-tag"), None);
    }

    #[test]
    fn an_unreadable_manifest_is_reported_by_the_scan() {
        let dir = tempfile::tempdir().expect("create temp dir");
        write_manifest(
            dir.path(),
            "registry.ollama.ai/library/qwen3/4b",
            &[MODEL_HEX],
        );
        let broken = dir
            .path()
            .join("manifests/registry.ollama.ai/library/broken/1b");
        std::fs::create_dir_all(broken.parent().expect("parent")).expect("mkdir");
        std::fs::write(&broken, b"{ half a manifest").expect("write broken manifest");

        let scan = manifest_scan(dir.path());

        assert_eq!(scan.manifests.len(), 1);
        assert_eq!(scan.unreadable, [broken]);
        assert_eq!(
            manifest_scan(&dir.path().join("nothing-here")),
            ManifestScan::default()
        );
    }

    #[test]
    fn unparseable_manifest_names_no_tag() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let broken = dir
            .path()
            .join("manifests/registry.ollama.ai/library/broken/1b");
        std::fs::create_dir_all(broken.parent().expect("parent")).expect("mkdir");
        std::fs::write(&broken, b"not json").expect("write broken manifest");

        assert!(manifests(dir.path()).is_empty());
    }
}
