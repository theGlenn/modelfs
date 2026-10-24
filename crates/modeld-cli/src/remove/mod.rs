//! `modeld rm`: remove a model from one app, or from every app and the store.
//!
//! Removal is planned before anything changes. A [`Plan`] lists each step,
//! anything that blocks the removal ([`Refusal`]), things worth knowing
//! ([`Note`]), files that go with the model ([`Collateral`]), and about how
//! much space it frees ([`Frees`]). Its id hashes what the plan depends on:
//! the steps, the refusals, every touched file's stamp, the Ollama manifests,
//! the Hugging Face links, and the model's references. Execution takes the
//! store lock, plans again, and refuses to run if the id changed, so what
//! runs is exactly what was shown.
//!
//! Every path must be a reference the registry knows, inside a folder modeld
//! scans now, a regular file still holding the model's bytes (a cached digest
//! for its current stamp, or a hash under the lock), and open in no process
//! (a loaded model is mapped into memory). Each app's files go the way that
//! app expects:
//!
//! - **Ollama**: through its API, which must listen on a loopback address
//!   and list each tag with the manifest found on disk. Ollama deletes every
//!   blob no remaining tag uses. A blob no manifest names is deleted
//!   directly, unless a partial download of it exists or it changed in the
//!   last 5 minutes.
//! - **Hugging Face**: snapshot links first, then the blob, so no link ever
//!   dangles; never while the download lock is held. The repo folder goes
//!   too once no tracked model file would remain in it.
//! - **LM Studio**: the file moves to the Trash, or its whole
//!   `publisher/repo` folder once no model file would remain in it.
//! - **Your folders**: the file moves to the Trash.
//! - **The stored copy** (removing everywhere): deleted last, only after
//!   every other step succeeded.
//!
//! Removing the last copy any app has removes the stored copy too. Steps run
//! in order and stop at the first failure or between steps on SIGINT. Then
//! the registry forgets every removed path, and journal entries of removed
//! files are dropped so `restore` and `gc` are not blocked by them.

use crate::inventory::{Presence, presence};
use accounting::Removal;
use guard::Guard;
use modeld_core::Digest;
use modeld_providers::ProviderRoot;
use modeld_store::{Reference, Store, StoreError};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

mod accounting;
mod checks;
mod files;
mod guard;
mod huggingface;
mod ollama;
#[cfg(test)]
mod tests;
pub mod text;

pub use accounting::Frees;
pub use files::trash;
pub use ollama::Api as OllamaApi;

/// Which copies of the models to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// These referenced paths.
    Paths(Vec<PathBuf>),
    /// Every copy one app holds: `ollama`, `huggingface`, `lmstudio`, or
    /// `manual` (your folders).
    Provider(String),
    /// Every copy, and the stored copy.
    Everywhere,
}

/// How one step removes a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Asks Ollama to delete a tag; it deletes blobs no other tag uses.
    OllamaDelete { tag: String },
    /// Unlinks a Hugging Face snapshot symlink.
    Unlink,
    /// Deletes a file now.
    Delete,
    /// Deletes a Hugging Face repo folder no tracked model file remains in.
    DeleteFolder,
    /// Moves a file to the Trash.
    Trash,
    /// Moves an LM Studio model folder to the Trash.
    TrashFolder,
    /// Deletes the store's copy and every registry row of the model.
    RemoveStoredCopy,
}

/// One planned change to the filesystem or the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub digest: Digest,
    /// The app the path belongs to, or `store`.
    pub provider: String,
    pub action: Action,
    pub path: PathBuf,
    /// Facts checked again right before the step runs.
    guards: Vec<Guard>,
}

/// Why a path blocks the removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    /// No stored model has this digest.
    UnknownModel,
    /// The path is not a reference of the model.
    NotReferenced,
    /// No copy matches the requested app.
    NothingSelected,
    /// The path is outside every folder modeld scans now.
    OutsideRoots,
    /// The path is not a regular file (a symlink, a folder).
    NotRegularFile,
    /// The path cannot be checked.
    Unreadable,
    /// The file no longer holds the model's bytes.
    ContentChanged,
    /// A process has the file open.
    InUse,
    /// The file is being downloaded.
    DownloadInProgress,
    /// Ollama's API does not answer.
    OllamaNotRunning,
    /// `OLLAMA_HOST` points at another machine.
    OllamaNotLoopback,
    /// The running Ollama does not list the tag as found on disk.
    OllamaStoreMismatch,
}

/// A path that blocks the removal, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: RefusalCode,
    pub path: PathBuf,
    pub message: String,
    /// The processes holding the file open, for [`RefusalCode::InUse`].
    pub processes: Vec<String>,
}

/// Something worth knowing before confirming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteCode {
    /// The last app's copy goes, so the stored copy goes too.
    LastApp,
    /// The file is already gone; only the registry forgets it.
    AlreadyGone,
    /// The file changed stamp since it was hashed; it is hashed again first.
    VerifyByHashing,
    /// The file is inside a git repository.
    GitRepository,
    /// Symlinks point at the file and will stop working.
    Symlinks,
}

/// A note attached to a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub code: NoteCode,
    pub path: PathBuf,
    pub message: String,
    /// Related paths: the git repository, or the symlinks.
    pub paths: Vec<PathBuf>,
}

/// A file that goes because of the removal without being a model copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collateral {
    pub path: PathBuf,
    pub size: u64,
    pub reason: String,
}

/// A model the plan removes copies of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedModel {
    pub digest: Digest,
    pub name: String,
    pub size: u64,
    /// Every copy goes, and the stored copy with them.
    pub everywhere: bool,
}

/// Everything a removal would do, and what blocks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Changes whenever anything the plan depends on changes.
    pub id: String,
    pub models: Vec<PlannedModel>,
    pub steps: Vec<Step>,
    pub refusals: Vec<Refusal>,
    pub notes: Vec<Note>,
    pub collateral: Vec<Collateral>,
    pub frees: Frees,
    /// Every reference the plan removes, already-gone ones included.
    targets: Vec<PathBuf>,
}

/// What executing a plan did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Indexes of the steps that ran to completion, in order.
    pub completed: Vec<usize>,
    /// The step that failed, and why; later steps did not run.
    pub failure: Option<(usize, String)>,
    /// A stop signal ended the removal between steps.
    pub stopped: bool,
    /// Bookkeeping problems after the steps.
    pub warnings: Vec<String>,
}

/// A referenced path the plan may remove, checked.
#[derive(Debug, Clone)]
struct Target {
    digest: Digest,
    reference: Reference,
    root: ProviderRoot,
    stamp: modeld_store::FileStamp,
}

impl Target {
    /// The file must keep its checked stamp and stay closed until removed.
    fn guard(&self) -> Guard {
        Guard::File {
            path: self.reference.path.clone(),
            stamp: self.stamp,
        }
    }
}

/// Builds plans against the store and the folders modeld scans.
#[derive(Debug)]
pub struct Planner<'a> {
    pub store: &'a Store,
    pub roots: &'a [ProviderRoot],
    pub ollama: &'a OllamaApi,
    /// Hash files whose digest is not cached; only under the store lock.
    pub verify: bool,
}

impl Planner<'_> {
    /// Plans removing the copies `scope` selects of every model in `digests`.
    ///
    /// `on_hashing` hears each file hashed to prove its content.
    ///
    /// # Errors
    /// The registry could not be read.
    pub fn plan(
        &self,
        digests: &[Digest],
        scope: &Scope,
        on_hashing: &mut dyn FnMut(&Path, u64),
    ) -> Result<Plan, StoreError> {
        let mut draft = Draft::default();
        for digest in digests {
            self.plan_model(digest, scope, &mut draft, on_hashing)?;
        }
        let by_provider = draft.take_targets();
        ollama::plan(by_provider.get("ollama"), self.ollama, &mut draft);
        huggingface::plan(by_provider.get("huggingface"), &mut draft);
        files::plan_lmstudio(by_provider.get("lmstudio"), &mut draft);
        files::plan_manual(by_provider.get("manual"), self.roots, &mut draft);
        for model in draft.models.clone().iter().filter(|model| model.everywhere) {
            draft.steps.push(Step {
                digest: model.digest.clone(),
                provider: "store".to_string(),
                action: Action::RemoveStoredCopy,
                path: self.store.blob_path(&model.digest),
                guards: Vec::new(),
            });
        }
        let frees = accounting::frees(
            self.store,
            &draft.models,
            &draft.removals,
            &draft.collateral,
        );
        Ok(draft.finish(frees))
    }

    fn plan_model(
        &self,
        digest: &Digest,
        scope: &Scope,
        draft: &mut Draft,
        on_hashing: &mut dyn FnMut(&Path, u64),
    ) -> Result<(), StoreError> {
        let Some(artifact) = self.store.artifact(digest)? else {
            draft.refuse(
                RefusalCode::UnknownModel,
                Path::new(&digest.to_string()),
                "no stored model has this digest; run `modeld sync` first?",
            );
            return Ok(());
        };
        let references = &artifact.references;
        let mut selected: Vec<&Reference> = match scope {
            Scope::Paths(paths) => {
                let wanted: Vec<PathBuf> = paths.iter().map(|path| resolved(path)).collect();
                let names = |reference: &Reference| {
                    paths.contains(&reference.path) || wanted.contains(&resolved(&reference.path))
                };
                for (path, resolved_path) in paths.iter().zip(&wanted) {
                    let known = references.iter().any(|reference| {
                        reference.path == *path || resolved(&reference.path) == *resolved_path
                    });
                    if !known {
                        draft.refuse(
                            RefusalCode::NotReferenced,
                            path,
                            "not a copy of this model that modeld knows",
                        );
                    }
                }
                references
                    .iter()
                    .filter(|reference| names(reference))
                    .collect()
            }
            Scope::Provider(provider) => references
                .iter()
                .filter(|reference| reference.provider == *provider)
                .collect(),
            Scope::Everywhere => references.iter().collect(),
        };
        if selected.is_empty() && *scope != Scope::Everywhere {
            draft.refuse(
                RefusalCode::NothingSelected,
                Path::new(&digest.to_string()),
                "no copy of this model matches",
            );
            return Ok(());
        }
        // A copy on an unplugged drive still counts: it is not gone.
        let present = |reference: &&Reference| presence(&reference.path) != Presence::Gone;
        let everywhere = *scope == Scope::Everywhere
            || references
                .iter()
                .filter(present)
                .all(|reference| selected.iter().any(|chosen| chosen.path == reference.path));
        if everywhere && *scope != Scope::Everywhere {
            draft.note(
                NoteCode::LastApp,
                &self.store.blob_path(digest),
                "no other app has a copy, so the stored copy goes too",
                Vec::new(),
            );
            selected = references.iter().collect();
        }
        for reference in selected {
            self.plan_reference(digest, reference, draft, on_hashing);
        }
        for reference in references {
            draft.fingerprint(format!("reference|{}", reference.path.display()));
        }
        draft.fingerprint(format!("model|{digest}|everywhere={everywhere}"));
        draft.models.push(PlannedModel {
            digest: digest.clone(),
            name: crate::report::display_name(&artifact),
            size: artifact.size,
            everywhere,
        });
        Ok(())
    }

    fn plan_reference(
        &self,
        digest: &Digest,
        reference: &Reference,
        draft: &mut Draft,
        on_hashing: &mut dyn FnMut(&Path, u64),
    ) {
        draft.targets.push(reference.path.clone());
        match presence(&reference.path) {
            Presence::Gone => {
                draft.note(
                    NoteCode::AlreadyGone,
                    &reference.path,
                    "already gone; modeld only forgets it",
                    Vec::new(),
                );
                return;
            }
            Presence::Offline => {
                draft.refuse(
                    RefusalCode::Unreadable,
                    &reference.path,
                    "on a drive that is not connected; connect it first",
                );
                return;
            }
            Presence::Present => {}
        }
        let context = checks::Context {
            store: self.store,
            roots: self.roots,
            verify: self.verify,
        };
        match checks::check(&context, digest, reference, on_hashing) {
            Ok(checked) => {
                draft.fingerprint(checked.fingerprint);
                if let Some(note) = checked.note {
                    draft.notes.push(note);
                }
                draft.checked.push(Target {
                    digest: digest.clone(),
                    reference: reference.clone(),
                    root: checked.root,
                    stamp: checked.stamp,
                });
            }
            Err(refusal) => draft.refusals.push(refusal),
        }
    }
}

/// A plan being assembled.
#[derive(Debug, Default)]
struct Draft {
    models: Vec<PlannedModel>,
    targets: Vec<PathBuf>,
    checked: Vec<Target>,
    steps: Vec<Step>,
    refusals: Vec<Refusal>,
    notes: Vec<Note>,
    collateral: Vec<(Collateral, Removal)>,
    /// How each removed reference goes, for the freed-space estimate.
    removals: HashMap<PathBuf, Removal>,
    fingerprint: Vec<String>,
}

impl Draft {
    fn refuse(&mut self, code: RefusalCode, path: &Path, message: impl Into<String>) {
        self.refusals.push(Refusal {
            code,
            path: path.to_path_buf(),
            message: message.into(),
            processes: Vec::new(),
        });
    }

    fn note(
        &mut self,
        code: NoteCode,
        path: &Path,
        message: impl Into<String>,
        paths: Vec<PathBuf>,
    ) {
        self.notes.push(Note {
            code,
            path: path.to_path_buf(),
            message: message.into(),
            paths,
        });
    }

    fn fingerprint(&mut self, line: String) {
        self.fingerprint.push(line);
    }

    fn step(&mut self, target: &Target, action: Action, path: &Path, guards: Vec<Guard>) {
        self.steps.push(Step {
            digest: target.digest.clone(),
            provider: target.reference.provider.clone(),
            action,
            path: path.to_path_buf(),
            guards,
        });
    }

    /// Hands the checked targets out, grouped by app.
    fn take_targets(&mut self) -> BTreeMap<String, Vec<Target>> {
        let mut grouped: BTreeMap<String, Vec<Target>> = BTreeMap::new();
        for target in std::mem::take(&mut self.checked) {
            grouped
                .entry(target.reference.provider.clone())
                .or_default()
                .push(target);
        }
        grouped
    }

    fn finish(mut self, frees: Frees) -> Plan {
        for step in &self.steps {
            self.fingerprint.push(format!(
                "step|{:?}|{}|{}",
                step.action,
                step.digest,
                step.path.display()
            ));
        }
        for refusal in &self.refusals {
            self.fingerprint.push(format!(
                "refusal|{:?}|{}",
                refusal.code,
                refusal.path.display()
            ));
        }
        // What goes along must be what was shown: a file appearing in a
        // folder after the dry run changes the id.
        for (item, _) in &self.collateral {
            self.fingerprint
                .push(format!("collateral|{}|{}", item.path.display(), item.size));
        }
        let mut hasher = sha2::Sha256::new();
        for line in &self.fingerprint {
            hasher.update(line.as_bytes());
            hasher.update(b"\n");
        }
        let id = hex::encode(&hasher.finalize()[..8]);
        Plan {
            id,
            models: self.models,
            steps: self.steps,
            refusals: self.refusals,
            notes: self.notes,
            collateral: self.collateral.into_iter().map(|(item, _)| item).collect(),
            frees,
            targets: self.targets,
        }
    }
}

/// Runs plans: the store, Ollama's API, and how files go to the Trash.
#[derive(Debug)]
pub struct Executor<'a> {
    pub store: &'a Store,
    pub ollama: &'a OllamaApi,
    /// [`trash`] in production; tests keep the user's Trash out of it.
    pub trash: fn(&Path) -> Result<(), String>,
}

impl Executor<'_> {
    /// Runs `plan`'s steps in order, then updates the registry and journal.
    ///
    /// The caller holds the store lock and has re-planned under it. Steps
    /// stop at the first failure, and between steps once `stop` is raised;
    /// the stored copy is always the last step, so it only goes when
    /// everything else did. `on_step` hears each step before it runs.
    pub fn run(&self, plan: &Plan, stop: &AtomicBool, mut on_step: impl FnMut(&Step)) -> Outcome {
        let mut outcome = Outcome::default();
        let _download_locks = match huggingface::hold_locks(&plan.steps) {
            Ok(locks) => locks,
            Err(reason) => {
                outcome.failure = Some((0, reason));
                return outcome;
            }
        };
        for (index, step) in plan.steps.iter().enumerate() {
            if stop.load(Ordering::Relaxed) {
                outcome.stopped = true;
                break;
            }
            on_step(step);
            if let Err(reason) = self.perform(step) {
                outcome.failure = Some((index, reason));
                break;
            }
            outcome.completed.push(index);
        }
        outcome.warnings = forget_removed(plan, self.store);
        outcome
    }

    fn perform(&self, step: &Step) -> Result<(), String> {
        for guard in &step.guards {
            guard.check(self.ollama)?;
        }
        match &step.action {
            Action::OllamaDelete { tag } => {
                self.ollama.delete(tag)?;
                if step.path.exists() {
                    return Err(format!("Ollama kept the manifest of {tag}"));
                }
                Ok(())
            }
            Action::Unlink => files::unlink(&step.path),
            Action::Delete => files::delete_file(&step.path),
            Action::DeleteFolder => files::delete_folder(&step.path),
            Action::Trash | Action::TrashFolder => (self.trash)(&step.path),
            Action::RemoveStoredCopy => self
                .store
                .remove_artifact(&step.digest)
                .map_err(|error| error.to_string()),
        }
    }
}

/// Forgets every removed path and drops its journal entries.
///
/// Covers the targets, the files that went along (an Ollama layer may be
/// another model's reference), and every step's path. Only paths gone from
/// a mounted volume are forgotten.
fn forget_removed(plan: &Plan, store: &Store) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut candidates: Vec<&PathBuf> = plan
        .targets
        .iter()
        .chain(plan.collateral.iter().map(|item| &item.path))
        .chain(plan.steps.iter().map(|step| &step.path))
        .collect();
    candidates.sort();
    candidates.dedup();
    let gone: Vec<&PathBuf> = candidates
        .into_iter()
        .filter(|path| presence(path) == Presence::Gone)
        .collect();
    for path in &gone {
        if let Err(error) = store.remove_reference(path) {
            warnings.push(format!("could not forget {} ({error})", path.display()));
        }
    }
    let journal = modeld_core::consolidate::Journal::open(store.root().join("journal.jsonl"));
    let result = journal.and_then(|journal| {
        let entries = journal.entries()?;
        let kept: Vec<_> = entries
            .iter()
            .filter(|entry| !gone.contains(&&entry.victim))
            .cloned()
            .collect();
        if kept.len() == entries.len() {
            return Ok(());
        }
        journal.rewrite(&kept)
    });
    if let Err(error) = result {
        warnings.push(format!("could not update the journal ({error})"));
    }
    warnings
}

/// `path` with symlinks resolved, as scans record it (`/private/var` for
/// `/var`). A missing file resolves through its folder.
fn resolved(path: &Path) -> PathBuf {
    path.canonicalize()
        .ok()
        .or_else(|| {
            let folder = path.parent()?.canonicalize().ok()?;
            Some(folder.join(path.file_name()?))
        })
        .unwrap_or_else(|| path.to_path_buf())
}
