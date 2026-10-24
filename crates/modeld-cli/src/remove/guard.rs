//! What a step checks again right before it runs.
//!
//! The plan is made under the store lock, but steps run one after another,
//! and an Ollama delete can take seconds: an app may rewrite a file, open
//! it, or re-pull a tag meanwhile. Each step carries the facts it depends
//! on, and refuses to run if one no longer holds; later steps then do not
//! run either.

use super::OllamaApi;
use super::checks::processes_using;
use crate::inventory;
use modeld_core::Digest;
use modeld_providers::ollama::manifest_scan;
use modeld_store::FileStamp;
use std::path::PathBuf;

/// A fact the plan relied on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Guard {
    /// The file keeps the stamp it was checked at, and no process has it
    /// open. An earlier step may have removed it already.
    File { path: PathBuf, stamp: FileStamp },
    /// Ollama still lists the tag with this manifest (SHA-256, in hex).
    OllamaTag { tag: String, manifest: String },
    /// No manifest under the models root names the blob, and every
    /// manifest there can be read.
    OllamaOrphan { root: PathBuf, digest: Digest },
}

impl Guard {
    /// Checks the fact again.
    ///
    /// # Errors
    /// Why the step must not run.
    pub(super) fn check(&self, api: &OllamaApi) -> Result<(), String> {
        match self {
            Self::File { path, stamp } => {
                match FileStamp::of(path) {
                    Ok(now) if now == *stamp => {}
                    Ok(_) => {
                        return Err(format!(
                            "{} changed since the plan was made",
                            path.display()
                        ));
                    }
                    Err(error) if inventory::is_gone(&error) => return Ok(()),
                    Err(error) => return Err(format!("cannot check {} ({error})", path.display())),
                }
                let processes = processes_using(path)?;
                if processes.is_empty() {
                    return Ok(());
                }
                Err(format!(
                    "{} is open in {}",
                    path.display(),
                    processes.join(", ")
                ))
            }
            Self::OllamaTag { tag, manifest } => match api.listed_manifest(tag)? {
                Some(listed) if listed == *manifest => Ok(()),
                _ => Err(format!("Ollama's {tag} changed since the plan was made")),
            },
            Self::OllamaOrphan { root, digest } => {
                let scan = manifest_scan(root);
                if let Some(unreadable) = scan.unreadable.first() {
                    return Err(format!(
                        "cannot read the Ollama manifest {}",
                        unreadable.display()
                    ));
                }
                if scan
                    .manifests
                    .iter()
                    .any(|manifest| manifest.digests.contains(digest))
                {
                    return Err("an Ollama tag now uses this blob".to_string());
                }
                Ok(())
            }
        }
    }
}
