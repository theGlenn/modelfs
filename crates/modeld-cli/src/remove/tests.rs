//! Removal against real fixture trees for each app's layout.

use super::ollama::tests::{FakeOllama, remote_ollama, stopped_ollama};
use super::*;
use modeld_core::{Algorithm, ProviderKind};
use std::fs::File;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, SystemTime};

/// Files under this size stay out of the store, like Ollama license blobs.
const MIN_SIZE: u64 = 32;
const WEIGHTS: &[u8] = b"model weights, long enough to be tracked by modeld";

/// A home folder with a store, scan roots, and a private Trash.
struct Home {
    dir: tempfile::TempDir,
    store: Store,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = Store::open(dir.path().join(".modeld")).expect("open store");
        Self { dir, store }
    }

    fn root(&self, kind: ProviderKind, relative: &str) -> ProviderRoot {
        let root = self.dir.path().join(relative);
        std::fs::create_dir_all(&root).expect("create root");
        ProviderRoot {
            kind,
            root,
            excluded: vec![],
            label_prefix: None,
        }
    }

    fn sync(&self, roots: &[ProviderRoot]) -> Vec<modeld_core::Artifact> {
        let scanned = modeld_providers::scan::scan(roots, MIN_SIZE);
        crate::sync::run(&self.store, scanned, Duration::ZERO, |_, _| {}).synced
    }

    /// Syncs, then turns every duplicate into a clone of its blob.
    fn deduplicate(&self, roots: &[ProviderRoot]) {
        let synced = self.sync(roots);
        let plan = crate::consolidation::plan_against_store(&self.store, &synced);
        crate::consolidation::apply(
            &self.store,
            &self.journal(),
            &plan,
            |_| {},
            |w| panic!("{w}"),
        );
    }

    fn journal(&self) -> modeld_core::consolidate::Journal {
        modeld_core::consolidate::Journal::open(self.dir.path().join(".modeld/journal.jsonl"))
            .expect("journal")
    }

    fn plan(&self, roots: &[ProviderRoot], api: &OllamaApi, scope: &Scope, verify: bool) -> Plan {
        let planner = Planner {
            store: &self.store,
            roots,
            ollama: api,
            verify,
        };
        planner
            .plan(&[digest(WEIGHTS)], scope, &mut |_, _| {})
            .expect("plan")
    }

    fn execute(&self, plan: &Plan, api: &OllamaApi) -> Outcome {
        self.execute_with(plan, api, test_trash)
    }

    fn execute_with(
        &self,
        plan: &Plan,
        api: &OllamaApi,
        trash: fn(&Path) -> Result<(), String>,
    ) -> Outcome {
        let executor = Executor {
            store: &self.store,
            ollama: api,
            trash,
        };
        executor.run(plan, &AtomicBool::new(false), |_| {})
    }

    fn artifact(&self) -> Option<modeld_store::StoredArtifact> {
        self.store.artifact(&digest(WEIGHTS)).expect("registry")
    }
}

/// Writes `content` at `path`, backdated past every settle window.
fn settled(path: &Path, content: &[u8]) -> PathBuf {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, content).expect("write");
    let old = SystemTime::now() - Duration::from_hours(1);
    File::open(path)
        .and_then(|file| file.set_times(std::fs::FileTimes::new().set_modified(old)))
        .expect("backdate");
    path.to_path_buf()
}

fn digest(content: &[u8]) -> Digest {
    Digest::new(Algorithm::Sha256, sha2::Sha256::digest(content).to_vec()).expect("digest")
}

/// Moves files into a per-process folder instead of the user's Trash.
fn test_trash(path: &Path) -> Result<(), String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let trash = std::env::temp_dir().join(format!("modeld-test-trash-{}", std::process::id()));
    std::fs::create_dir_all(&trash).map_err(|error| error.to_string())?;
    let name = format!("{}", NEXT.fetch_add(1, Ordering::Relaxed));
    std::fs::rename(path, trash.join(name)).map_err(|error| error.to_string())
}

fn failing_trash(_: &Path) -> Result<(), String> {
    Err("the Trash is full".to_string())
}

fn actions(plan: &Plan) -> Vec<&Action> {
    plan.steps.iter().map(|step| &step.action).collect()
}

fn codes(plan: &Plan) -> Vec<RefusalCode> {
    plan.refusals.iter().map(|refusal| refusal.code).collect()
}

/// A hub-cache repo holding `WEIGHTS` with snapshot links in two commits.
struct HubRepo {
    root: ProviderRoot,
    repo: PathBuf,
    blob: PathBuf,
    links: Vec<PathBuf>,
}

fn hub_repo(home: &Home) -> HubRepo {
    let root = home.root(ProviderKind::HuggingFace, "hf/hub");
    let repo = root.root.join("models--org--tiny");
    let blob = settled(&repo.join("blobs").join(digest(WEIGHTS).to_hex()), WEIGHTS);
    let config = settled(&repo.join("blobs").join(digest(b"{}").to_hex()), b"{}");
    let mut links = Vec::new();
    for commit in ["aaa", "bbb"] {
        let link = repo.join("snapshots").join(commit).join("model.gguf");
        std::fs::create_dir_all(link.parent().expect("parent")).expect("mkdir");
        std::os::unix::fs::symlink(&blob, &link).expect("link");
        links.push(link);
    }
    std::os::unix::fs::symlink(&config, repo.join("snapshots/aaa/config.json")).expect("link");
    HubRepo {
        root,
        repo,
        blob,
        links,
    }
}

#[test]
fn hugging_face_links_go_before_their_blob_then_the_emptied_repo() {
    let home = Home::new();
    let hub = hub_repo(&home);
    let roots = [hub.root.clone()];
    home.sync(&roots);

    let plan = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);
    let outcome = home.execute(&plan, &stopped_ollama());

    assert_eq!(
        actions(&plan),
        [
            &Action::Unlink,
            &Action::Unlink,
            &Action::Delete,
            &Action::DeleteFolder,
            &Action::RemoveStoredCopy
        ]
    );
    assert_eq!(plan.steps[0].path, hub.links[0]);
    let collateral: Vec<&PathBuf> = plan.collateral.iter().map(|item| &item.path).collect();
    assert_eq!(
        collateral,
        [
            &hub.repo.join("blobs").join(digest(b"{}").to_hex()),
            &hub.repo.join("snapshots/aaa/config.json")
        ],
        "the small config blob and its link go too"
    );
    assert_eq!(outcome.failure, None);
    assert!(!hub.repo.exists());
    assert!(home.artifact().is_none());
}

#[test]
fn a_repo_keeping_another_model_keeps_its_folder_and_no_link_dangles() {
    let home = Home::new();
    let hub = hub_repo(&home);
    let other = vec![3u8; 1_100_000];
    let other_blob = settled(
        &hub.repo.join("blobs").join(digest(&other).to_hex()),
        &other,
    );
    std::os::unix::fs::symlink(&other_blob, hub.repo.join("snapshots/aaa/other.gguf"))
        .expect("link");
    let roots = [hub.root.clone()];
    home.sync(&roots);

    let plan = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);
    home.execute(&plan, &stopped_ollama());

    assert!(!actions(&plan).contains(&&Action::DeleteFolder));
    assert!(!hub.blob.exists());
    assert!(other_blob.exists());
    let dangling: Vec<PathBuf> = walkdir::WalkDir::new(hub.repo.join("snapshots"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(walkdir::DirEntry::path_is_symlink)
        .map(walkdir::DirEntry::into_path)
        .filter(|link| !link.exists())
        .collect();
    assert!(dangling.is_empty(), "{dangling:?}");
}

#[test]
fn a_held_download_lock_blocks_the_blob() {
    let home = Home::new();
    let hub = hub_repo(&home);
    let roots = [hub.root.clone()];
    home.sync(&roots);
    let lock = modeld_providers::huggingface::lock_file(&hub.blob).expect("lock path");
    std::fs::create_dir_all(lock.parent().expect("parent")).expect("mkdir");
    let held = File::create(&lock).expect("create lock");
    held.lock().expect("hold lock");

    let plan = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);

    assert_eq!(codes(&plan), [RefusalCode::DownloadInProgress]);
    assert!(!actions(&plan).contains(&&Action::Delete));
}

#[test]
fn an_open_file_is_refused_as_in_use() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    let model = settled(&root.root.join("a.gguf"), WEIGHTS);
    let roots = [root];
    home.sync(&roots);
    let _open = File::open(&model).expect("open");

    let plan = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);

    assert_eq!(codes(&plan), [RefusalCode::InUse]);
    assert!(!plan.refusals[0].processes.is_empty());
}

/// An Ollama models root with `WEIGHTS` and a license under two tags.
fn ollama_models(home: &Home) -> (ProviderRoot, Vec<(String, PathBuf)>, PathBuf) {
    let root = home.root(ProviderKind::Ollama, "ollama/models");
    let blob = settled(
        &root
            .root
            .join("blobs")
            .join(format!("sha256-{}", digest(WEIGHTS).to_hex())),
        WEIGHTS,
    );
    let license = b"MIT";
    settled(
        &root
            .root
            .join("blobs")
            .join(format!("sha256-{}", digest(license).to_hex())),
        license,
    );
    let manifest = format!(
        r#"{{"schemaVersion":2,"layers":[{{"digest":"{}"}},{{"digest":"{}"}}]}}"#,
        digest(WEIGHTS),
        digest(license)
    );
    let mut tags = Vec::new();
    for tag in ["4b", "latest"] {
        let path = root
            .root
            .join("manifests/registry.ollama.ai/library/qwen3")
            .join(tag);
        settled(&path, manifest.as_bytes());
        tags.push((format!("qwen3:{tag}"), path));
    }
    (root, tags, blob)
}

#[test]
fn ollama_tags_are_deleted_through_its_api() {
    let home = Home::new();
    let (root, tags, blob) = ollama_models(&home);
    let ollama = FakeOllama::serve(root.root.clone(), tags);
    let roots = [root];
    home.sync(&roots);

    let plan = home.plan(&roots, &ollama.api, &Scope::Everywhere, false);
    let outcome = home.execute(&plan, &ollama.api);

    assert_eq!(
        actions(&plan),
        [
            &Action::OllamaDelete {
                tag: "qwen3:4b".to_string()
            },
            &Action::OllamaDelete {
                tag: "qwen3:latest".to_string()
            },
            &Action::RemoveStoredCopy
        ]
    );
    assert_eq!(plan.collateral.len(), 1, "the license only these tags use");
    assert_eq!(outcome.failure, None);
    assert_eq!(
        *ollama.deleted.lock().expect("lock"),
        ["qwen3:4b", "qwen3:latest"]
    );
    assert!(!blob.exists());
    assert!(home.artifact().is_none());
}

#[test]
fn ollama_must_be_running_and_on_this_machine() {
    let home = Home::new();
    let (root, _, _) = ollama_models(&home);
    let roots = [root];
    home.sync(&roots);

    let stopped = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);
    let remote = home.plan(&roots, &remote_ollama(), &Scope::Everywhere, false);

    assert_eq!(codes(&stopped), [RefusalCode::OllamaNotRunning]);
    assert_eq!(codes(&remote), [RefusalCode::OllamaNotLoopback]);
}

#[test]
fn an_ollama_listing_other_manifests_is_a_store_mismatch() {
    let home = Home::new();
    let (root, tags, _) = ollama_models(&home);
    let elsewhere = settled(
        &home.dir.path().join("other-store/manifest"),
        b"{\"layers\":[]}",
    );
    let listed = tags
        .into_iter()
        .map(|(name, _)| (name, elsewhere.clone()))
        .collect();
    let ollama = FakeOllama::serve(home.dir.path().join("other-store"), listed);
    let roots = [root];
    home.sync(&roots);

    let plan = home.plan(&roots, &ollama.api, &Scope::Everywhere, false);

    assert_eq!(codes(&plan), [RefusalCode::OllamaStoreMismatch]);
}

#[test]
fn an_orphan_ollama_blob_is_deleted_unless_still_downloading() {
    let home = Home::new();
    let root = home.root(ProviderKind::Ollama, "ollama/models");
    let hex = digest(WEIGHTS).to_hex();
    let blob = settled(
        &root.root.join("blobs").join(format!("sha256-{hex}")),
        WEIGHTS,
    );
    let roots = [root.clone()];
    home.sync(&roots);

    let idle = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);
    settled(
        &root
            .root
            .join("blobs")
            .join(format!("sha256-{hex}-partial-0")),
        b"x",
    );
    let downloading = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);

    assert_eq!(idle.steps[0].action, Action::Delete);
    assert_eq!(idle.steps[0].path, blob);
    assert_eq!(codes(&downloading), [RefusalCode::DownloadInProgress]);
}

#[test]
fn an_lmstudio_folder_goes_to_the_trash_once_it_holds_no_model() {
    let home = Home::new();
    let lmstudio = home.root(ProviderKind::LmStudio, "lmstudio/models");
    let manual = home.root(ProviderKind::Manual, "models");
    let model = settled(&lmstudio.root.join("pub/repo/model.gguf"), WEIGHTS);
    settled(&lmstudio.root.join("pub/repo/config.json"), b"{}");
    settled(&manual.root.join("copy.gguf"), WEIGHTS);
    let roots = [lmstudio.clone(), manual];
    home.sync(&roots);

    let plan = home.plan(&roots, &stopped_ollama(), &Scope::Paths(vec![model]), false);

    assert_eq!(actions(&plan), [&Action::TrashFolder]);
    assert_eq!(plan.steps[0].path, lmstudio.root.join("pub/repo"));
    assert!(!plan.models[0].everywhere);
    assert_eq!(plan.collateral.len(), 1);
}

#[test]
fn removing_one_of_several_shared_copies_frees_about_nothing() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    let first = settled(&root.root.join("a.gguf"), WEIGHTS);
    settled(&root.root.join("b.gguf"), WEIGHTS);
    let roots = [root];
    home.deduplicate(&roots);

    let plan = home.plan(&roots, &stopped_ollama(), &Scope::Paths(vec![first]), false);

    assert_eq!(plan.frees.now + plan.frees.after_trash, 0);
    assert_eq!(plan.frees.pinned, WEIGHTS.len() as u64);
}

#[test]
fn removing_the_last_copy_takes_the_stored_copy_too() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    let model = settled(&root.root.join("a.gguf"), WEIGHTS);
    let roots = [root];
    home.sync(&roots);

    let plan = home.plan(
        &roots,
        &stopped_ollama(),
        &Scope::Paths(vec![model.clone()]),
        false,
    );
    let outcome = home.execute(&plan, &stopped_ollama());

    assert!(plan.models[0].everywhere);
    assert_eq!(plan.notes[0].code, NoteCode::LastApp);
    assert_eq!(actions(&plan), [&Action::Trash, &Action::RemoveStoredCopy]);
    assert_eq!(plan.frees.after_trash, WEIGHTS.len() as u64);
    assert_eq!(outcome.completed, [0, 1]);
    assert!(!model.exists());
    assert!(home.artifact().is_none());
}

#[test]
fn a_failed_step_keeps_the_stored_copy() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    settled(&root.root.join("a.gguf"), WEIGHTS);
    settled(&root.root.join("b.gguf"), WEIGHTS);
    let roots = [root];
    home.sync(&roots);

    let plan = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);
    let outcome = home.execute_with(&plan, &stopped_ollama(), failing_trash);

    assert_eq!(outcome.failure.map(|(step, _)| step), Some(0));
    assert!(outcome.completed.is_empty());
    assert!(home.store.blob_path(&digest(WEIGHTS)).exists());
    assert_eq!(home.artifact().expect("artifact").references.len(), 2);
}

#[test]
fn a_file_rewritten_after_the_dry_run_changes_the_plan() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    let model = settled(&root.root.join("a.gguf"), WEIGHTS);
    settled(&root.root.join("b.gguf"), WEIGHTS);
    let roots = [root];
    home.sync(&roots);
    let dry = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);

    let mut retrained = WEIGHTS.to_vec();
    retrained[0] ^= 1;
    settled(&model, &retrained);
    let locked = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, true);

    assert!(dry.refusals.is_empty());
    assert_ne!(dry.id, locked.id);
    assert_eq!(codes(&locked), [RefusalCode::ContentChanged]);
}

#[test]
fn the_same_state_plans_the_same_id() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    settled(&root.root.join("a.gguf"), WEIGHTS);
    let roots = [root];
    home.sync(&roots);

    let dry = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);
    let locked = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, true);

    assert_eq!(dry.id, locked.id);
}

#[test]
fn deduplicate_then_remove_a_copy_then_restore_succeeds() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    settled(&root.root.join("a.gguf"), WEIGHTS);
    settled(&root.root.join("b.gguf"), WEIGHTS);
    let roots = [root];
    home.deduplicate(&roots);
    let replaced = home.journal().entries().expect("journal")[0].victim.clone();

    let plan = home.plan(
        &roots,
        &stopped_ollama(),
        &Scope::Paths(vec![replaced]),
        false,
    );
    home.execute(&plan, &stopped_ollama());
    let restored = modeld_core::consolidate::restore(&home.journal()).expect("restore");

    assert!(home.journal().entries().expect("journal").is_empty());
    assert!(restored.refused.is_empty());
}

#[test]
fn a_path_that_is_not_a_copy_of_the_model_is_refused() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    settled(&root.root.join("a.gguf"), WEIGHTS);
    let stranger = settled(
        &root.root.join("other.bin"),
        b"something else entirely, long enough",
    );
    let roots = [root];
    home.sync(&roots);

    let plan = home.plan(
        &roots,
        &stopped_ollama(),
        &Scope::Paths(vec![stranger]),
        false,
    );

    assert!(codes(&plan).contains(&RefusalCode::NotReferenced));
}

#[test]
fn a_path_given_through_a_symlinked_folder_still_names_its_copy() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    let model = settled(&root.root.join("a.gguf"), WEIGHTS);
    settled(&root.root.join("b.gguf"), WEIGHTS);
    let alias = home.dir.path().join("alias");
    std::os::unix::fs::symlink(&root.root, &alias).expect("alias");
    let roots = [root];
    home.sync(&roots);

    let plan = home.plan(
        &roots,
        &stopped_ollama(),
        &Scope::Paths(vec![alias.join("a.gguf")]),
        false,
    );

    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    assert_eq!(plan.steps[0].path, model);
}

#[test]
fn files_in_a_git_repository_and_linked_files_get_notes() {
    let home = Home::new();
    let project = home.dir.path().join("project");
    std::fs::create_dir_all(project.join(".git")).expect("mkdir .git");
    let root = home.root(ProviderKind::Manual, "project/models");
    let fixtures = home.root(ProviderKind::Manual, "fixtures");
    let model = settled(&root.root.join("a.gguf"), WEIGHTS);
    std::os::unix::fs::symlink(&model, fixtures.root.join("linked.gguf")).expect("link");
    let roots = [root, fixtures];
    home.sync(&roots);

    let plan = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);

    let notes: Vec<NoteCode> = plan.notes.iter().map(|note| note.code).collect();
    assert!(notes.contains(&NoteCode::GitRepository), "{notes:?}");
    assert!(notes.contains(&NoteCode::Symlinks), "{notes:?}");
}

#[test]
fn a_copy_on_an_unplugged_drive_keeps_the_stored_copy() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    let model = settled(&root.root.join("a.gguf"), WEIGHTS);
    let roots = [root];
    home.sync(&roots);
    let unplugged = PathBuf::from("/Volumes/modeld-test-unplugged/models/a.gguf");
    home.store
        .record_reference(
            &digest(WEIGHTS),
            &unplugged,
            ProviderKind::LmStudio,
            None,
            1,
        )
        .expect("record reference");

    let one_app = home.plan(&roots, &stopped_ollama(), &Scope::Paths(vec![model]), false);
    let everywhere = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);

    assert!(
        !one_app.models[0].everywhere,
        "the drive's copy still exists"
    );
    assert!(!actions(&one_app).contains(&&Action::RemoveStoredCopy));
    assert_eq!(codes(&everywhere), [RefusalCode::Unreadable]);
}

#[test]
fn an_unreadable_ollama_manifest_blocks_deleting_an_orphan() {
    let home = Home::new();
    let root = home.root(ProviderKind::Ollama, "ollama/models");
    let hex = digest(WEIGHTS).to_hex();
    settled(
        &root.root.join("blobs").join(format!("sha256-{hex}")),
        WEIGHTS,
    );
    settled(
        &root
            .root
            .join("manifests/registry.ollama.ai/library/half/written"),
        b"{ \"layers\": [",
    );
    let roots = [root];
    home.sync(&roots);

    let plan = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);

    assert_eq!(codes(&plan), [RefusalCode::Unreadable]);
    assert!(!actions(&plan).contains(&&Action::Delete));
}

#[test]
fn a_file_rewritten_after_planning_stops_its_step() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    let model = settled(&root.root.join("a.gguf"), WEIGHTS);
    let roots = [root];
    home.sync(&roots);
    let plan = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, true);

    settled(&model, WEIGHTS);
    let outcome = home.execute(&plan, &stopped_ollama());

    let (step, reason) = outcome.failure.expect("refused to run");
    assert_eq!(step, 0);
    assert!(reason.contains("changed since the plan"), "{reason}");
    assert!(model.exists());
    assert!(home.store.blob_path(&digest(WEIGHTS)).exists());
}

#[test]
fn a_re_pulled_ollama_tag_is_not_deleted() {
    let home = Home::new();
    let (root, tags, blob) = ollama_models(&home);
    let ollama = FakeOllama::serve(root.root.clone(), tags.clone());
    let roots = [root];
    home.sync(&roots);
    let plan = home.plan(&roots, &ollama.api, &Scope::Everywhere, true);

    let manifest = &tags[0].1;
    let mut repulled = std::fs::read(manifest).expect("read manifest");
    repulled.push(b' ');
    std::fs::write(manifest, repulled).expect("rewrite manifest");
    let outcome = home.execute(&plan, &ollama.api);

    assert!(
        outcome
            .failure
            .is_some_and(|(_, reason)| reason.contains("changed"))
    );
    assert!(ollama.deleted.lock().expect("lock").is_empty());
    assert!(blob.exists());
}

#[test]
fn an_lmstudio_folder_that_cannot_be_read_in_full_stays() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    let lmstudio = home.root(ProviderKind::LmStudio, "lmstudio/models");
    let model = settled(&lmstudio.root.join("pub/repo/model.gguf"), WEIGHTS);
    let locked = lmstudio.root.join("pub/repo/split");
    settled(
        &locked.join("other.gguf"),
        b"another model, unreadable to the walk",
    );
    let roots = [lmstudio];
    home.sync(&roots);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock");

    let plan = home.plan(
        &roots,
        &stopped_ollama(),
        &Scope::Paths(vec![model.clone()]),
        false,
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock");

    assert_eq!(actions(&plan), [&Action::Trash, &Action::RemoveStoredCopy]);
    assert_eq!(plan.steps[0].path, model);
}

#[test]
fn the_stores_own_files_are_never_removed() {
    let home = Home::new();
    let root = home.root(ProviderKind::Manual, "models");
    settled(&root.root.join("a.gguf"), WEIGHTS);
    let roots = [root];
    home.sync(&roots);
    let blob = home.store.blob_path(&digest(WEIGHTS));
    home.store
        .record_reference(&digest(WEIGHTS), &blob, ProviderKind::Manual, None, 1)
        .expect("record reference");
    let store_root = ProviderRoot {
        kind: ProviderKind::Manual,
        root: home.dir.path().to_path_buf(),
        excluded: vec![],
        label_prefix: None,
    };

    let plan = home.plan(
        &[store_root],
        &stopped_ollama(),
        &Scope::Paths(vec![blob]),
        false,
    );

    assert_eq!(codes(&plan), [RefusalCode::OutsideRoots]);
    assert!(plan.refusals[0].message.contains("store"));
}

#[test]
fn a_hugging_face_row_outside_a_repo_is_refused() {
    let home = Home::new();
    let hub = home.root(ProviderKind::HuggingFace, "hf/hub");
    let stray = settled(&hub.root.join("stray.gguf"), WEIGHTS);
    let manual = home.root(ProviderKind::Manual, "models");
    settled(&manual.root.join("a.gguf"), WEIGHTS);
    home.sync(std::slice::from_ref(&manual));
    home.store
        .record_reference(&digest(WEIGHTS), &stray, ProviderKind::HuggingFace, None, 1)
        .expect("record reference");

    let plan = home.plan(
        &[hub, manual],
        &stopped_ollama(),
        &Scope::Provider("huggingface".to_string()),
        false,
    );

    assert_eq!(codes(&plan), [RefusalCode::OutsideRoots]);
    assert!(!actions(&plan).contains(&&Action::DeleteFolder));
}

#[test]
fn a_file_appearing_in_a_folder_that_goes_changes_the_plan() {
    let home = Home::new();
    let hub = hub_repo(&home);
    let roots = [hub.root.clone()];
    home.sync(&roots);
    let before = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);

    settled(&hub.repo.join("blobs/late-download"), b"x");
    let after = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);

    assert_ne!(before.id, after.id);
}

#[test]
fn a_tracked_file_that_goes_along_is_forgotten_too() {
    let home = Home::new();
    let hub = hub_repo(&home);
    let tokenizer = b"tokenizer vocabulary that is tracked as its own model";
    let tokenizer_blob = settled(
        &hub.repo.join("blobs").join(digest(tokenizer).to_hex()),
        tokenizer,
    );
    let roots = [hub.root.clone()];
    home.sync(&roots);

    let plan = home.plan(&roots, &stopped_ollama(), &Scope::Everywhere, false);
    home.execute(&plan, &stopped_ollama());

    assert!(
        plan.collateral
            .iter()
            .any(|item| item.path == tokenizer_blob)
    );
    let other = home
        .store
        .artifact(&digest(tokenizer))
        .expect("registry")
        .expect("artifact");
    assert!(other.references.is_empty(), "{:?}", other.references);
}
