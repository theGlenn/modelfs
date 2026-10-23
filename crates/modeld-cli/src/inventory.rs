//! What the store holds and where each copy stands now, for `ls --json`.
//!
//! The registry records which paths held each digest at the last sync. The
//! inventory checks every one of those paths on disk again and gives it a
//! [`ReferenceState`]: gone, sharing the stored copy's blocks, hard-linked,
//! changed since it was hashed, or an independent copy a pass could clone.
//!
//! Disk accounting ([`Space`]) follows from those states. It replaces the
//! old "logical minus physical" figure, which counted every reference as
//! sharing even before any clone was made. Clones of one blob share one set
//! of blocks; every other present copy is counted at full size, hard links
//! once per inode. The numbers are estimates: a clone made by another tool
//! between two references, or a clone that diverged between the sampled
//! offsets, is not seen.
//!
//! Reading the inventory is read-only: nothing is recorded, hashed, or
//! adopted. A file found sharing the blob's blocks without a clone record
//! is reported as a clone; the next pass records it.

use crate::report;
use modeld_core::{Digest, FileId};
use modeld_providers::{Detection, ProviderRoot, huggingface, lmstudio, ollama};
use modeld_store::{FileStamp, Reference, Store, StoreError, StoredArtifact};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ops::AddAssign;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Where one referenced path stands against the stored copy of its digest.
///
/// Ordered from gone to independent copy, the order `ls` checks them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceState {
    /// The path no longer exists; the next complete sync drops it.
    Missing,
    /// An APFS clone of the stored copy: it shares the stored blocks.
    Clone,
    /// Other names share this file's inode, or it is the stored copy
    /// itself. modeld never replaces such a file.
    Hardlinked,
    /// The file changed since modeld hashed it, so its bytes are unknown.
    Changed,
    /// An independent copy of the stored bytes; a pass can clone it.
    Copy,
}

/// Disk usage of models, in bytes, estimated from reference states.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Space {
    /// What model copies and stored copies occupy on disk.
    pub on_disk: u64,
    /// What sharing saves: clones of one stored copy beyond the first.
    pub saved: u64,
    /// What a pass could still free by cloning independent copies.
    pub pending: u64,
    /// Stored copies of models no app references anymore.
    pub store_only: u64,
    /// What the apps' copies would occupy without any sharing.
    pub logical: u64,
}

impl AddAssign for Space {
    fn add_assign(&mut self, other: Self) {
        self.on_disk += other.on_disk;
        self.saved += other.saved;
        self.pending += other.pending;
        self.store_only += other.store_only;
        self.logical += other.logical;
    }
}

/// The store's canonical copy of one digest.
#[derive(Debug, Clone)]
pub struct StoredCopy {
    pub path: PathBuf,
    /// The blob's identity; `None` when the blob file is missing.
    pub id: Option<FileId>,
}

/// One referenced path, checked on disk now.
#[derive(Debug, Clone)]
pub struct Located {
    pub reference: Reference,
    pub state: ReferenceState,
    /// Where the copy came from in its app's terms: the Hugging Face repo,
    /// the LM Studio `publisher/repo`, the Ollama tags, or the root of one
    /// of your folders.
    pub source: Option<String>,
    /// Every Ollama tag whose manifest uses the blob; empty for other apps.
    pub tags: Vec<String>,
    id: Option<FileId>,
}

/// One stored model with its references and their disk usage.
#[derive(Debug, Clone)]
pub struct Model {
    pub artifact: StoredArtifact,
    pub display_name: String,
    pub stored: StoredCopy,
    pub references: Vec<Located>,
    pub space: Space,
}

/// Every stored model, largest first, with overall disk usage.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub models: Vec<Model>,
    pub totals: Space,
}

impl Inventory {
    /// Reads the registry and checks every referenced path on disk.
    ///
    /// `detection` supplies the roots used to name each copy's source.
    ///
    /// # Errors
    /// The registry could not be read.
    pub fn read(store: &Store, detection: &Detection) -> Result<Self, StoreError> {
        let mut sources = Sources::new(&detection.roots);
        let mut inventory = Self::default();
        for artifact in store.artifacts()? {
            let model = Model::locate(store, artifact, &mut sources);
            inventory.totals += model.space;
            inventory.models.push(model);
        }
        Ok(inventory)
    }
}

impl Model {
    fn locate(store: &Store, artifact: StoredArtifact, sources: &mut Sources<'_>) -> Self {
        let blob = store.blob_path(&artifact.digest);
        let stored = StoredCopy {
            id: std::fs::metadata(&blob)
                .ok()
                .map(|metadata| file_id(&metadata)),
            path: blob,
        };
        let references: Vec<Located> = artifact
            .references
            .iter()
            .map(|reference| {
                let (state, id) = locate(store, &artifact.digest, &stored, &reference.path);
                let (source, tags) = sources.of(reference, &artifact.digest);
                Located {
                    reference: reference.clone(),
                    state,
                    source,
                    tags,
                    id,
                }
            })
            .collect();
        let copies: Vec<(ReferenceState, Option<FileId>)> = references
            .iter()
            .map(|located| (located.state, located.id))
            .collect();
        Self {
            display_name: report::display_name(&artifact),
            space: space(artifact.size, stored.id, &copies),
            artifact,
            stored,
            references,
        }
    }
}

/// Checks one referenced path against the stored copy of `digest`.
///
/// Order matters: a recorded clone is trusted only while its stamp matches,
/// and the sampled block comparison only runs on a file whose stamp still
/// matches its verified digest.
fn locate(
    store: &Store,
    digest: &Digest,
    stored: &StoredCopy,
    path: &Path,
) -> (ReferenceState, Option<FileId>) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return (ReferenceState::Missing, None);
    };
    let id = Some(file_id(&metadata));
    if !metadata.is_file() {
        return (ReferenceState::Changed, id);
    }
    if store.path_is_shared(digest, path).unwrap_or(false) {
        return (ReferenceState::Clone, id);
    }
    if metadata.nlink() > 1 || (stored.id.is_some() && id == stored.id) {
        return (ReferenceState::Hardlinked, id);
    }
    let stamp = FileStamp::from_metadata(&metadata);
    let verified = store
        .cached_digest(path, stamp)
        .ok()
        .flatten()
        .is_some_and(|cached| cached == *digest);
    if !verified {
        return (ReferenceState::Changed, id);
    }
    let shares_blob = stored.id.is_some()
        && modeld_core::apfs::shares_extents(path, &stored.path).unwrap_or(false);
    if shares_blob {
        return (ReferenceState::Clone, id);
    }
    (ReferenceState::Copy, id)
}

/// Disk usage of one stored artifact of `size` bytes.
///
/// The stored copy, its clones, and any path hard-linked to either form one
/// physical copy. Every other present path holds its own, counted once per
/// inode. Only independent copies are pending: a hard-linked file is never
/// replaced, and a changed one may no longer match.
fn space(size: u64, blob: Option<FileId>, copies: &[(ReferenceState, Option<FileId>)]) -> Space {
    let is_anchored = |state: ReferenceState, id: Option<FileId>| {
        state == ReferenceState::Clone || (blob.is_some() && id == blob)
    };
    let present = count(
        copies
            .iter()
            .filter(|(state, _)| *state != ReferenceState::Missing),
    );
    let anchored = count(copies.iter().filter(|(state, id)| is_anchored(*state, *id)));
    let anchored_ids: HashSet<FileId> = copies
        .iter()
        .filter(|(state, id)| is_anchored(*state, *id))
        .filter_map(|(_, id)| *id)
        .chain(blob)
        .collect();
    let independent: HashSet<FileId> = copies
        .iter()
        .filter(|(state, _)| *state != ReferenceState::Missing)
        .filter_map(|(_, id)| *id)
        .filter(|id| !anchored_ids.contains(id))
        .collect();
    let pending = count(
        copies
            .iter()
            .filter(|(state, _)| *state == ReferenceState::Copy),
    );
    Space {
        on_disk: size * (u64::from(!anchored_ids.is_empty()) + count(independent.iter())),
        saved: size * anchored.saturating_sub(1),
        pending: size * pending,
        store_only: if present == 0 && blob.is_some() {
            size
        } else {
            0
        },
        logical: size * present,
    }
}

fn count<T>(items: impl Iterator<Item = T>) -> u64 {
    u64::try_from(items.count()).unwrap_or(u64::MAX)
}

fn file_id(metadata: &std::fs::Metadata) -> FileId {
    FileId {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

/// Names each reference's source, reading each Ollama root's manifests once.
struct Sources<'a> {
    roots: &'a [ProviderRoot],
    ollama: HashMap<PathBuf, Vec<ollama::ManifestInfo>>,
}

impl<'a> Sources<'a> {
    fn new(roots: &'a [ProviderRoot]) -> Self {
        Self {
            roots,
            ollama: HashMap::new(),
        }
    }

    /// The display source of `reference`, and its Ollama tags if any.
    fn of(&mut self, reference: &Reference, digest: &Digest) -> (Option<String>, Vec<String>) {
        let path = &reference.path;
        match reference.provider.as_str() {
            "ollama" => {
                let tags = self.ollama_tags(path, digest);
                ((!tags.is_empty()).then(|| tags.join(", ")), tags)
            }
            "huggingface" => (huggingface::repo_of_blob(path), Vec::new()),
            "lmstudio" => (self.lmstudio_repo(path), Vec::new()),
            _ => (self.manual_root(path), Vec::new()),
        }
    }

    /// Tags whose manifest lists `digest`, for a blob at `<root>/blobs/<name>`.
    fn ollama_tags(&mut self, blob: &Path, digest: &Digest) -> Vec<String> {
        let Some(root) = blob.parent().and_then(Path::parent) else {
            return Vec::new();
        };
        let manifests = self
            .ollama
            .entry(root.to_path_buf())
            .or_insert_with(|| ollama::manifests(root));
        let mut tags: Vec<String> = manifests
            .iter()
            .filter(|manifest| manifest.digests.contains(digest))
            .map(|manifest| manifest.api_name.clone())
            .collect();
        tags.sort_unstable();
        tags.dedup();
        tags
    }

    fn lmstudio_repo(&self, file: &Path) -> Option<String> {
        let root = self.root_containing(file)?;
        let folder = lmstudio::model_folder(&root.root, file)?;
        Some(folder.strip_prefix(&root.root).ok()?.display().to_string())
    }

    fn manual_root(&self, file: &Path) -> Option<String> {
        let root = self.root_containing(file)?;
        Some(
            root.label_prefix
                .clone()
                .unwrap_or_else(|| root.root.display().to_string()),
        )
    }

    /// The most specific detected root holding `file`.
    fn root_containing(&self, file: &Path) -> Option<&'a ProviderRoot> {
        self.roots
            .iter()
            .filter(|root| file.starts_with(&root.root))
            .max_by_key(|root| root.root.as_os_str().len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::tests::Fixture;
    use std::time::Duration;

    const SIZE: u64 = 100;
    const BLOB: Option<FileId> = Some(FileId {
        device: 1,
        inode: 1,
    });

    /// A present copy in `state` with its own inode.
    fn at(state: ReferenceState, inode: u64) -> (ReferenceState, Option<FileId>) {
        (state, Some(FileId { device: 1, inode }))
    }

    #[test]
    fn two_clones_of_a_stored_copy_save_one_copy() {
        let copies = [at(ReferenceState::Clone, 10), at(ReferenceState::Clone, 11)];

        let space = space(SIZE, BLOB, &copies);

        assert_eq!(space.on_disk, SIZE);
        assert_eq!(space.saved, SIZE);
        assert_eq!(space.pending, 0);
        assert_eq!(space.logical, 2 * SIZE);
    }

    #[test]
    fn independent_copies_are_pending_not_saved() {
        let copies = [
            at(ReferenceState::Clone, 10),
            at(ReferenceState::Copy, 11),
            at(ReferenceState::Copy, 12),
        ];

        let space = space(SIZE, BLOB, &copies);

        assert_eq!(space.on_disk, 3 * SIZE);
        assert_eq!(space.saved, 0);
        assert_eq!(space.pending, 2 * SIZE);
    }

    #[test]
    fn stored_copy_without_clones_occupies_its_own_blocks() {
        let copies = [at(ReferenceState::Copy, 11)];

        let space = space(SIZE, BLOB, &copies);

        assert_eq!(space.on_disk, 2 * SIZE);
        assert_eq!(space.pending, SIZE);
        assert_eq!(space.store_only, 0);
    }

    #[test]
    fn model_no_app_references_is_store_only() {
        let copies = [(ReferenceState::Missing, None)];

        let space = space(SIZE, BLOB, &copies);

        assert_eq!(space.store_only, SIZE);
        assert_eq!(space.on_disk, SIZE);
        assert_eq!(space.logical, 0);
    }

    #[test]
    fn hard_link_to_a_clone_shares_the_clone_blocks() {
        let copies = [
            at(ReferenceState::Clone, 20),
            at(ReferenceState::Hardlinked, 20),
        ];

        assert_eq!(space(SIZE, BLOB, &copies).on_disk, SIZE);
    }

    #[test]
    fn hard_links_count_once_and_are_never_pending() {
        let copies = [
            at(ReferenceState::Hardlinked, 20),
            at(ReferenceState::Hardlinked, 20),
            at(ReferenceState::Changed, 21),
        ];

        let space = space(SIZE, BLOB, &copies);

        assert_eq!(space.on_disk, 3 * SIZE);
        assert_eq!(space.pending, 0);
    }

    #[test]
    fn clones_keep_sharing_after_the_blob_is_deleted() {
        let copies = [at(ReferenceState::Clone, 10), at(ReferenceState::Clone, 11)];

        assert_eq!(space(SIZE, None, &copies).on_disk, SIZE);
        assert_eq!(space(SIZE, None, &[]).on_disk, 0);
    }

    fn states(fixture: &Fixture) -> Vec<(PathBuf, ReferenceState)> {
        let inventory = Inventory::read(&fixture.store, &Detection::default()).expect("inventory");
        let mut states: Vec<_> = inventory
            .models
            .iter()
            .flat_map(|model| &model.references)
            .map(|located| (located.reference.path.clone(), located.state))
            .collect();
        states.sort();
        states
    }

    /// Just the states, sorted: which file sync imports first depends on
    /// directory order.
    fn state_set(fixture: &Fixture) -> Vec<ReferenceState> {
        let mut states: Vec<_> = states(fixture)
            .into_iter()
            .map(|(_, state)| state)
            .collect();
        states.sort();
        states
    }

    #[test]
    fn synced_copies_are_a_clone_and_an_independent_copy() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");
        fixture.settled_model("b.gguf", b"weights");

        fixture.sync(Duration::ZERO);

        // The import source shares the blob; the other copy does not yet.
        assert_eq!(
            state_set(&fixture),
            [ReferenceState::Clone, ReferenceState::Copy]
        );
    }

    #[test]
    fn deleted_and_rewritten_files_are_missing_and_changed() {
        let fixture = Fixture::new();
        let deleted = fixture.settled_model("a.gguf", b"weights");
        let rewritten = fixture.settled_model("b.gguf", b"weights");
        fixture.sync(Duration::ZERO);

        std::fs::remove_file(&deleted).expect("delete");
        std::fs::write(&rewritten, b"retrained").expect("rewrite");

        assert_eq!(
            states(&fixture),
            [
                (deleted, ReferenceState::Missing),
                (rewritten, ReferenceState::Changed)
            ]
        );
    }

    #[test]
    fn hard_link_of_the_import_source_is_reported_as_such() {
        let fixture = Fixture::new();
        let source = fixture.settled_model("a.gguf", b"weights");
        std::fs::hard_link(&source, fixture.root.root.join("b.gguf")).expect("hard link");

        fixture.sync(Duration::ZERO);

        // The blob was cloned from the inode both names share.
        assert_eq!(
            state_set(&fixture),
            [ReferenceState::Clone, ReferenceState::Hardlinked]
        );
    }

    #[test]
    fn unrecorded_clone_of_the_blob_reads_as_a_clone() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");
        let digest = fixture.sync(Duration::ZERO).synced[0]
            .digest
            .clone()
            .expect("digest");
        let untracked = fixture.root.root.join("untracked.gguf");
        modeld_core::apfs::clone_file(&fixture.store.blob_path(&digest), &untracked)
            .expect("clone blob");
        fixture.sync(Duration::ZERO);

        let state = states(&fixture)
            .into_iter()
            .find(|(path, _)| *path == untracked)
            .map(|(_, state)| state);

        assert_eq!(state, Some(ReferenceState::Clone));
    }

    #[test]
    fn consolidated_copies_add_up_to_one_saved_copy() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");
        fixture.settled_model("b.gguf", b"weights");
        let synced = fixture.sync(Duration::ZERO).synced;
        let plan = crate::consolidation::plan_against_store(&fixture.store, &synced);
        let journal = modeld_core::consolidate::Journal::open(
            fixture.dir.path().join(".modeld/journal.jsonl"),
        )
        .expect("journal");
        crate::consolidation::apply(&fixture.store, &journal, &plan, |_| {}, |w| panic!("{w}"));

        let inventory = Inventory::read(&fixture.store, &Detection::default()).expect("inventory");

        let size = b"weights".len() as u64;
        assert_eq!(inventory.totals.saved, size);
        assert_eq!(inventory.totals.on_disk, size);
        assert_eq!(inventory.totals.pending, 0);
    }

    #[test]
    fn manual_copies_name_their_root() {
        let fixture = Fixture::new();
        fixture.settled_model("a.gguf", b"weights");
        fixture.sync(Duration::ZERO);
        let root = ProviderRoot {
            label_prefix: Some("ws/roseau/fixtures/models".to_string()),
            ..fixture.root.clone()
        };
        let detection = Detection {
            roots: vec![root],
            unreadable: vec![],
        };

        let inventory = Inventory::read(&fixture.store, &detection).expect("inventory");

        assert_eq!(
            inventory.models[0].references[0].source.as_deref(),
            Some("ws/roseau/fixtures/models")
        );
    }

    #[test]
    fn ollama_blob_lists_every_tag_using_it() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let hex = "797b70c4edf85907fe0a49eb85811256f65fa0f7bf52166b147fd16be2be4662";
        for tag in ["4b", "latest"] {
            let manifest = dir
                .path()
                .join("manifests/registry.ollama.ai/library/qwen3")
                .join(tag);
            std::fs::create_dir_all(manifest.parent().expect("parent")).expect("mkdir");
            std::fs::write(
                manifest,
                format!(r#"{{"layers":[{{"digest":"sha256:{hex}"}}]}}"#),
            )
            .expect("write manifest");
        }
        let blob = dir.path().join("blobs").join(format!("sha256-{hex}"));
        let digest: Digest = format!("sha256:{hex}").parse().expect("digest");
        let reference = Reference {
            path: blob,
            provider: "ollama".to_string(),
            label: None,
        };

        let (source, tags) = Sources::new(&[]).of(&reference, &digest);

        assert_eq!(tags, ["qwen3:4b", "qwen3:latest"]);
        assert_eq!(source.as_deref(), Some("qwen3:4b, qwen3:latest"));
    }
}
