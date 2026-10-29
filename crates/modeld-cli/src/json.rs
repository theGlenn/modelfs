//! Machine-readable output: `--json` writes JSON Lines to stdout.
//!
//! Every line is one JSON object with a `type` and the [`SCHEMA_VERSION`]:
//!
//! - `{"type":"event","schema":1,"event":{"kind":"hashing",...}}` reports
//!   progress while a command runs (`pass`, `rm`, and `discover`);
//! - `{"type":"result","schema":1,"result":{...}}` is the command's outcome,
//!   always the last line of a successful run (exit code 0);
//! - `{"type":"error","schema":1,"error":{"code":"...","message":"..."}}`
//!   ends a failed run (exit code 1).
//!
//! Example lines for every command live in `schema/v1/`, and the rules for
//! changing the format in `schema/README.md`. The structs here are the
//! format: they are separate from the store's types, so a registry change
//! cannot silently change what the Mac app reads.
//!
//! Writing never panics and never stops a command. Write errors are dropped,
//! and `--json` keeps `SIGPIPE` ignored: if the reader quits (the app was
//! closed mid-pass), the pass still runs to the end and journals every swap.
//! Events go through an [`EventWriter`], so a reader that stops reading
//! without quitting cannot block a pass either.

use crate::inventory::{self, ReferenceState};
use crate::launchd;
use crate::reconcile::{PassEvent, PassReport};
use modeld_providers::Detection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::mpsc::SyncSender;
use std::thread::JoinHandle;

/// Version of the line format; bumped only for incompatible changes.
pub const SCHEMA_VERSION: u32 = 1;

/// One line of JSON output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Line<T> {
    Event { schema: u32, event: Event },
    Result { schema: u32, result: T },
    Error { schema: u32, error: Failure },
}

/// Writes the result line: the command's outcome.
pub fn emit_result<T: Serialize>(result: T) {
    write_line(&Line::Result {
        schema: SCHEMA_VERSION,
        result,
    });
}

/// Writes the error line: why the command failed.
pub fn emit_error(code: ErrorCode, message: impl Into<String>) {
    write_line(&Line::<()>::Error {
        schema: SCHEMA_VERSION,
        error: Failure {
            code,
            message: message.into(),
        },
    });
}

/// Serializes `line` and writes it whole to stdout, ignoring errors.
fn write_line<T: Serialize>(line: &T) {
    if let Some(text) = line_text(line) {
        write_text(&text);
    }
}

/// One serialized line with its newline.
fn line_text<T: Serialize>(line: &T) -> Option<String> {
    let mut text = serde_json::to_string(line).ok()?;
    text.push('\n');
    Some(text)
}

/// Writes `text` to a locked stdout in one call, ignoring errors.
fn write_text(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// How many event lines may wait for a slow reader before new ones drop.
const EVENT_BACKLOG: usize = 1024;

/// Writes event lines from a thread of its own, dropping them if the
/// reader falls far behind.
///
/// A pass reports events while it holds the store lock. Written directly,
/// a reader that stops reading but keeps the pipe open would block the
/// pass, and with it the daemon and every other mutating command. Events
/// are only progress, so dropping some is harmless. Result and error lines
/// are written after the pass has released the lock, so they may block.
#[derive(Debug)]
pub struct EventWriter {
    queue: Option<SyncSender<String>>,
    writer: Option<JoinHandle<()>>,
}

impl EventWriter {
    /// Starts the writer thread; without one, events are written inline.
    pub fn start() -> Self {
        let (queue, lines) = std::sync::mpsc::sync_channel::<String>(EVENT_BACKLOG);
        let writer = std::thread::Builder::new()
            .name("json-events".to_string())
            .spawn(move || {
                for line in lines {
                    write_text(&line);
                }
            });
        match writer {
            Ok(writer) => Self {
                queue: Some(queue),
                writer: Some(writer),
            },
            Err(_) => Self {
                queue: None,
                writer: None,
            },
        }
    }

    /// Queues an event line, or drops it when the backlog is full.
    pub fn send(&self, event: Event) {
        let Some(text) = line_text(&Line::<()>::Event {
            schema: SCHEMA_VERSION,
            event,
        }) else {
            return;
        };
        match &self.queue {
            Some(queue) => {
                let _ = queue.try_send(text);
            }
            None => write_text(&text),
        }
    }

    /// Waits until every queued event is written, so lines stay in order.
    pub fn finish(mut self) {
        self.queue = None;
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

/// Why a command failed, for callers to branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The store directory or its registry could not be opened.
    StoreUnavailable,
    /// The registry could not be read.
    RegistryUnreadable,
    /// The pass could not start or run.
    PassFailed,
    /// The pass was stopped while it waited for the store lock.
    Cancelled,
    /// Installing the login agent failed.
    InstallFailed,
    /// Removing the login agent failed.
    UninstallFailed,
    /// The command has no JSON output.
    Unsupported,
    /// `rm --json` needs `--expect-plan` or `--yes`.
    ConfirmationRequired,
    /// A removal could not start.
    RemoveFailed,
    /// A watched folder could not be added.
    WatchFailed,
    /// Discovery could not start.
    DiscoverFailed,
}

/// The payload of an error line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub code: ErrorCode,
    /// Human-readable detail; not part of the contract.
    pub message: String,
}

/// Progress of a pass, a removal, or a discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// Another modeld process holds the store lock; the pass waits for it.
    WaitingForLock,
    /// The pass holds the lock. A caller must not interrupt it from here on.
    Locked,
    Hashing {
        path: String,
        size: u64,
    },
    Cloning {
        path: String,
        size: u64,
    },
    /// `rm`: a step is about to run on this path.
    Removing {
        path: String,
    },
    /// `discover`: how many folders it has looked through so far.
    Discovering {
        folders: usize,
    },
}

impl From<PassEvent<'_>> for Event {
    fn from(event: PassEvent<'_>) -> Self {
        match event {
            PassEvent::WaitingForLock => Self::WaitingForLock,
            PassEvent::Locked => Self::Locked,
            PassEvent::Hashing { path, size } => Self::Hashing {
                path: text(path),
                size,
            },
            PassEvent::Cloning { path, size } => Self::Cloning {
                path: text(path),
                size,
            },
        }
    }
}

/// `doctor`: where models are looked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doctor {
    /// The store directory, normally `~/.modeld`.
    pub store: String,
    pub roots: Vec<Root>,
    /// Root sources that could not be read, such as a broken config file.
    pub unreadable: Vec<Problem>,
}

/// A folder modeld scans for one app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    pub provider: String,
    pub path: String,
    pub excluded: Vec<String>,
}

/// `watch`: a folder or pattern saved in the scan configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    /// Canonical absolute path of the watched folder, or the pattern as saved.
    pub path: String,
    /// False when the folder was already configured.
    pub added: bool,
    pub config: String,
}

/// `discover`: folders no scan covers that hold large model files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discover {
    /// Most bytes to free first, then largest.
    pub suggestions: Vec<FolderSuggestion>,
    /// Files directly in the home folder or a project root: too broad to watch.
    pub loose: Vec<DiscoveredFile>,
    /// The smallest file size considered, in bytes.
    pub min_size: u64,
    pub folders_read: usize,
    /// Entries that could not be listed or inspected.
    pub unreadable: usize,
    /// Files and folders left alone because they are stored only in the cloud.
    pub cloud_only: usize,
}

/// One folder, or one glob pattern over sibling projects, worth watching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSuggestion {
    /// The glob pattern to add with `watch --pattern`; `null` to add the one
    /// folder in `folders` with `watch`.
    pub pattern: Option<String>,
    /// The folders holding `files`.
    pub folders: Vec<String>,
    pub size: u64,
    /// Bytes watching it would probably free.
    pub reclaimable: u64,
    /// Largest first.
    pub files: Vec<DiscoveredFile>,
}

/// A large model file no scan covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredFile {
    pub path: String,
    pub size: u64,
    /// Same size and sampled bytes as a stored model or another file found.
    pub likely_copy: bool,
    /// Bytes no other file shares: what deduplicating it would free. Zero
    /// for a copy that is already a clone.
    pub reclaimable: u64,
}

impl Discover {
    pub fn new(discovery: &modeld_providers::discover::Discovery, min_size: u64) -> Self {
        Self {
            suggestions: discovery
                .suggestions
                .iter()
                .map(|suggestion| FolderSuggestion {
                    pattern: suggestion.pattern.clone(),
                    folders: suggestion
                        .folders
                        .iter()
                        .map(|folder| text(folder))
                        .collect(),
                    size: suggestion.size(),
                    reclaimable: suggestion.reclaimable(),
                    files: suggestion.files.iter().map(DiscoveredFile::from).collect(),
                })
                .collect(),
            loose: discovery.loose.iter().map(DiscoveredFile::from).collect(),
            min_size,
            folders_read: discovery.folders_read,
            unreadable: discovery.unreadable,
            cloud_only: discovery.cloud_only,
        }
    }
}

impl From<&modeld_providers::discover::Candidate> for DiscoveredFile {
    fn from(file: &modeld_providers::discover::Candidate) -> Self {
        Self {
            path: text(&file.path),
            size: file.size,
            likely_copy: file.likely_copy,
            reclaimable: file.reclaimable,
        }
    }
}

/// A path paired with why it was left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    pub path: String,
    pub reason: String,
}

impl Doctor {
    pub fn new(store: &Path, detection: &Detection) -> Self {
        Self {
            store: text(store),
            roots: detection
                .roots
                .iter()
                .map(|root| Root {
                    provider: inventory::provider_name(root.kind).to_string(),
                    path: text(&root.root),
                    excluded: root.excluded.iter().map(|path| text(path)).collect(),
                })
                .collect(),
            unreadable: detection
                .unreadable
                .iter()
                .map(|skipped| Problem {
                    path: text(&skipped.path),
                    reason: skipped.reason.clone(),
                })
                .collect(),
        }
    }
}

/// `ls`: every stored model and where its copies stand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ls {
    /// Largest first.
    pub models: Vec<Model>,
    pub totals: Space,
}

/// One stored model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    /// `sha256:<hex>`, the model's identity.
    pub digest: String,
    /// The most human name known: header name, label, or short digest.
    pub name: String,
    /// `model` for weights, `asset` for tokenizers and the like; `null`
    /// until the next sync reads the header.
    pub kind: Option<String>,
    pub format: Option<String>,
    pub quant: Option<String>,
    pub params: Option<String>,
    pub architecture: Option<String>,
    pub size: u64,
    pub stored: Stored,
    pub references: Vec<Reference>,
    pub space: Space,
}

/// The store's own copy of a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    pub path: String,
    pub present: bool,
}

/// One app's copy of a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    pub path: String,
    /// `ollama`, `huggingface`, `lmstudio`, `xybrid`, or `manual` (your folders).
    pub provider: String,
    pub label: Option<String>,
    /// Where it came from, in the app's terms: the Hugging Face repo, the
    /// LM Studio `publisher/repo`, the Ollama tags, or your folder's root.
    pub source: Option<String>,
    /// Every Ollama tag using this blob; empty for other apps.
    pub tags: Vec<String>,
    pub state: ReferenceState,
}

/// Disk usage in bytes; see the `inventory` module for definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    pub on_disk: u64,
    pub saved: u64,
    pub pending: u64,
    pub store_only: u64,
    pub logical: u64,
}

impl From<inventory::Space> for Space {
    fn from(space: inventory::Space) -> Self {
        Self {
            on_disk: space.on_disk,
            saved: space.saved,
            pending: space.pending,
            store_only: space.store_only,
            logical: space.logical,
        }
    }
}

impl From<&inventory::Inventory> for Ls {
    fn from(inventory: &inventory::Inventory) -> Self {
        Self {
            models: inventory.models.iter().map(Model::from).collect(),
            totals: inventory.totals.into(),
        }
    }
}

impl From<&inventory::Model> for Model {
    fn from(model: &inventory::Model) -> Self {
        let artifact = &model.artifact;
        let semantics = artifact.semantics.as_ref();
        Self {
            digest: artifact.digest.to_string(),
            name: model.display_name.clone(),
            kind: semantics.map(|s| s.kind.clone()),
            format: artifact.format.clone(),
            quant: semantics.and_then(|s| s.quant.clone()),
            params: semantics.and_then(|s| s.params.clone()),
            architecture: semantics.and_then(|s| s.architecture.clone()),
            size: artifact.size,
            stored: Stored {
                path: text(&model.stored.path),
                present: model.stored.id.is_some(),
            },
            references: model
                .references
                .iter()
                .map(|located| Reference {
                    path: text(&located.reference.path),
                    provider: located.reference.provider.clone(),
                    label: located.reference.label.clone(),
                    source: located.source.clone(),
                    tags: located.tags.clone(),
                    state: located.state,
                })
                .collect(),
            space: model.space.into(),
        }
    }
}

/// `pass`: what one pass did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pass {
    pub dry_run: bool,
    /// Blobs newly copied into the store.
    pub imported: usize,
    /// Files whose references the pass recorded.
    pub synced: usize,
    /// Stale references dropped; `null` when the pass was incomplete.
    pub pruned: Option<usize>,
    /// Every model file was read, so stale references could be dropped.
    pub complete: bool,
    /// Duplicates the pass set out to replace, or would on a dry run;
    /// `cloned` and `refused` say what happened to each.
    pub planned: Vec<Planned>,
    pub cloned: Vec<String>,
    /// Bytes the clones made free.
    pub freed: u64,
    pub refused: Vec<Problem>,
    pub deferred: Vec<Deferred>,
    pub skipped: Vec<Problem>,
    pub warnings: Vec<String>,
    /// The one-line summary the daemon logs, such as `pass: nothing new`.
    pub summary: String,
}

/// A duplicate planned for replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Planned {
    pub path: String,
    pub digest: String,
    pub size: u64,
}

/// A file left for later because it was written moments ago.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deferred {
    pub path: String,
    pub settles_in_secs: u64,
}

impl Pass {
    pub fn new(report: &PassReport, dry_run: bool) -> Self {
        let consolidation = report.consolidation.as_ref();
        Self {
            dry_run,
            imported: report.sync.imported,
            synced: report.sync.synced.len(),
            pruned: report.sync.pruned,
            complete: report.sync.is_complete(),
            planned: report
                .planned
                .iter()
                .map(|planned| Planned {
                    path: text(&planned.victim),
                    digest: planned.digest.to_string(),
                    size: planned.size,
                })
                .collect(),
            cloned: consolidation
                .map(|done| done.completed.iter().map(|path| text(path)).collect())
                .unwrap_or_default(),
            freed: consolidation.map_or(0, |done| done.bytes_affected),
            refused: consolidation
                .map(|done| {
                    done.refused
                        .iter()
                        .map(|refusal| Problem {
                            path: text(&refusal.victim),
                            reason: refusal.reason.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            deferred: report
                .sync
                .deferred
                .iter()
                .map(|deferred| Deferred {
                    path: text(&deferred.path),
                    settles_in_secs: deferred.settles_in.as_secs(),
                })
                .collect(),
            skipped: report
                .sync
                .skipped
                .iter()
                .map(|skipped| Problem {
                    path: text(&skipped.path),
                    reason: skipped.reason.clone(),
                })
                .collect(),
            warnings: report
                .sync
                .warnings
                .iter()
                .chain(&report.warnings)
                .cloned()
                .collect(),
            summary: crate::daemon::summarize(report),
        }
    }
}

/// `daemon status`: the login agent and its last pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub state: AgentState,
    pub pid: Option<u32>,
    /// The agent's own copy of modeld.
    pub binary: String,
    /// Whether that copy matches the running modeld; `null` if unreadable.
    /// `false` means `daemon install` would update the agent.
    pub binary_is_current: Option<bool>,
    pub plist: String,
    pub log: String,
    /// The installed agent only logs the clones it would make.
    pub dry_run: bool,
    /// Variables the installed agent runs with, such as `HF_HOME`.
    pub environment: BTreeMap<String, String>,
    pub last_pass: Option<LastPass>,
    /// The log's last lines, oldest first.
    pub recent_log: Vec<String>,
}

/// The agent's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Running,
    /// Loaded into launchd, but no process right now.
    Loaded,
    /// Installed but not loaded; it starts at the next login.
    Installed,
    NotInstalled,
}

/// The last pass the daemon logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastPass {
    /// RFC 3339 UTC, such as `2026-09-23T21:45:21Z`.
    pub at: String,
    pub summary: String,
    pub failed: bool,
}

/// What `DaemonStatus::new` reads, gathered by the caller.
#[derive(Debug)]
pub struct AgentFacts<'a> {
    pub status: &'a launchd::AgentStatus,
    pub paths: &'a launchd::AgentPaths,
    pub installed: Option<&'a launchd::InstalledAgent>,
    pub binary_is_current: Option<bool>,
    /// The daemon log's contents, if readable.
    pub log: Option<&'a str>,
}

impl DaemonStatus {
    pub fn new(facts: &AgentFacts<'_>) -> Self {
        let (state, pid) = match facts.status.state() {
            launchd::AgentState::Running { pid } => (AgentState::Running, Some(pid)),
            launchd::AgentState::Loaded => (AgentState::Loaded, None),
            launchd::AgentState::Installed => (AgentState::Installed, None),
            launchd::AgentState::NotInstalled => (AgentState::NotInstalled, None),
        };
        let installed = facts.installed.cloned().unwrap_or_default();
        Self {
            state,
            pid,
            binary: text(&facts.paths.binary),
            binary_is_current: facts.binary_is_current,
            plist: text(&facts.paths.plist),
            log: text(&facts.paths.log),
            dry_run: installed.arguments.iter().any(|arg| arg == "--dry-run"),
            environment: installed.environment,
            last_pass: facts.log.and_then(launchd::last_pass).map(|last| LastPass {
                at: last.at.to_string(),
                summary: last.summary.to_string(),
                failed: last.failed,
            }),
            recent_log: facts
                .log
                .map(|log| {
                    launchd::recent_lines(log, launchd::RECENT_LOG_LINES)
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// `daemon install`: where the agent was installed, and its status after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInstall {
    pub binary: String,
    pub plist: String,
    pub log: String,
    pub status: DaemonStatus,
}

impl DaemonInstall {
    pub fn new(paths: &launchd::AgentPaths, status: DaemonStatus) -> Self {
        Self {
            binary: text(&paths.binary),
            plist: text(&paths.plist),
            log: text(&paths.log),
            status,
        }
    }
}

/// `daemon uninstall`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonUninstall {
    /// An agent was installed and is now removed.
    pub was_installed: bool,
}

/// `rm`: the plan, and what running it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rm {
    pub status: crate::rm::Status,
    pub plan: RemovalPlan,
    /// What ran; `null` when nothing did (planned, plan changed, refused).
    pub outcome: Option<RemovalOutcome>,
}

/// Everything a removal would do, and what blocks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalPlan {
    /// Pass back with `--expect-plan` to run exactly this plan.
    pub plan_id: String,
    pub models: Vec<RemovalModel>,
    /// In the order they run.
    pub steps: Vec<RemovalStep>,
    /// Anything here blocks the whole removal.
    pub refusals: Vec<RemovalRefusal>,
    pub notes: Vec<RemovalNote>,
    /// Files that go along without being model copies.
    pub collateral: Vec<RemovalCollateral>,
    pub frees: Frees,
}

/// A model the plan removes copies of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalModel {
    pub digest: String,
    pub name: String,
    pub size: u64,
    /// Every copy goes, and the stored copy too.
    pub everywhere: bool,
}

/// One planned change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalStep {
    pub action: StepAction,
    /// The app the path belongs to, or `store`.
    pub provider: String,
    pub path: String,
    /// The Ollama tag, for `ollama_delete`.
    pub tag: Option<String>,
    pub digest: String,
}

/// How a step removes its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepAction {
    OllamaDelete,
    Unlink,
    Delete,
    DeleteFolder,
    Trash,
    TrashFolder,
    RemoveStoredCopy,
}

/// A path that blocks the removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalRefusal {
    pub code: crate::remove::RefusalCode,
    pub path: String,
    pub message: String,
    /// For `in_use`: the processes holding the file open.
    pub processes: Vec<String>,
}

/// Something worth knowing before confirming.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalNote {
    pub code: crate::remove::NoteCode,
    pub path: String,
    pub message: String,
    pub paths: Vec<String>,
}

/// A file that goes along with the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalCollateral {
    pub path: String,
    pub size: u64,
    pub reason: String,
}

/// About how much space the removal frees, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frees {
    pub now: u64,
    pub after_trash: u64,
    /// Bytes of removed files that other files keep using.
    pub pinned: u64,
}

/// What running the plan did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalOutcome {
    /// Indexes into `plan.steps` that ran to completion.
    pub completed: Vec<usize>,
    /// The step that failed; nothing after it ran.
    pub failure: Option<StepFailure>,
    pub stopped: bool,
    pub warnings: Vec<String>,
}

/// A step that failed, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepFailure {
    pub step: usize,
    pub reason: String,
}

impl Rm {
    pub fn new(
        status: crate::rm::Status,
        plan: &crate::remove::Plan,
        outcome: Option<&crate::remove::Outcome>,
    ) -> Self {
        Self {
            status,
            plan: RemovalPlan::from(plan),
            outcome: outcome.map(|outcome| RemovalOutcome {
                completed: outcome.completed.clone(),
                failure: outcome.failure.as_ref().map(|(step, reason)| StepFailure {
                    step: *step,
                    reason: reason.clone(),
                }),
                stopped: outcome.stopped,
                warnings: outcome.warnings.clone(),
            }),
        }
    }
}

impl From<&crate::remove::Plan> for RemovalPlan {
    fn from(plan: &crate::remove::Plan) -> Self {
        Self {
            plan_id: plan.id.clone(),
            models: plan
                .models
                .iter()
                .map(|model| RemovalModel {
                    digest: model.digest.to_string(),
                    name: model.name.clone(),
                    size: model.size,
                    everywhere: model.everywhere,
                })
                .collect(),
            steps: plan.steps.iter().map(RemovalStep::from).collect(),
            refusals: plan
                .refusals
                .iter()
                .map(|refusal| RemovalRefusal {
                    code: refusal.code,
                    path: text(&refusal.path),
                    message: refusal.message.clone(),
                    processes: refusal.processes.clone(),
                })
                .collect(),
            notes: plan
                .notes
                .iter()
                .map(|note| RemovalNote {
                    code: note.code,
                    path: text(&note.path),
                    message: note.message.clone(),
                    paths: note.paths.iter().map(|path| text(path)).collect(),
                })
                .collect(),
            collateral: plan
                .collateral
                .iter()
                .map(|item| RemovalCollateral {
                    path: text(&item.path),
                    size: item.size,
                    reason: item.reason.clone(),
                })
                .collect(),
            frees: Frees {
                now: plan.frees.now,
                after_trash: plan.frees.after_trash,
                pinned: plan.frees.pinned,
            },
        }
    }
}

impl From<&crate::remove::Step> for RemovalStep {
    fn from(step: &crate::remove::Step) -> Self {
        use crate::remove::Action;
        let (action, tag) = match &step.action {
            Action::OllamaDelete { tag } => (StepAction::OllamaDelete, Some(tag.clone())),
            Action::Unlink => (StepAction::Unlink, None),
            Action::Delete => (StepAction::Delete, None),
            Action::DeleteFolder => (StepAction::DeleteFolder, None),
            Action::Trash => (StepAction::Trash, None),
            Action::TrashFolder => (StepAction::TrashFolder, None),
            Action::RemoveStoredCopy => (StepAction::RemoveStoredCopy, None),
        };
        Self {
            action,
            provider: step.provider.clone(),
            path: text(&step.path),
            tag,
            digest: step.digest.to_string(),
        }
    }
}

/// A path as JSON text; APFS names are always valid UTF-8.
fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// `schema/v1` at the repo root.
    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1")
    }

    /// Compares `rendered` to the fixture, or rewrites the fixture when
    /// `UPDATE_SCHEMA_FIXTURES=1` is set.
    fn assert_fixture(name: &str, rendered: &str) {
        let path = fixtures().join(name);
        if std::env::var_os("UPDATE_SCHEMA_FIXTURES").is_some_and(|value| value == "1") {
            std::fs::write(&path, rendered).expect("write fixture");
            return;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            rendered, expected,
            "{name} changed; review it, then rerun with UPDATE_SCHEMA_FIXTURES=1"
        );
    }

    /// Renders one line the way fixtures store it: indented, newline-ended.
    fn pretty<T: Serialize>(line: &Line<T>) -> String {
        let mut text = serde_json::to_string_pretty(line).expect("serialize");
        text.push('\n');
        text
    }

    fn result<T>(result: T) -> Line<T> {
        Line::Result {
            schema: SCHEMA_VERSION,
            result,
        }
    }

    /// Decodes every line of `text` back and checks nothing was lost.
    fn assert_round_trips<T>(text: &str)
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let value: serde_json::Value = serde_json::from_str(text).expect("valid JSON");
        let decoded: Line<T> = serde_json::from_value(value.clone()).expect("decodes");
        assert_eq!(serde_json::to_value(&decoded).expect("encode"), value);
    }

    const HOME: &str = "/Users/me";
    const QWEN: &str = "sha256:04b1c301231dd422b8860db31311ab2721511346a32cb1e079c4c4e5f1fe4696";
    const MINILM: &str = "sha256:797b70c4edf85907fe0a49eb85811256f65fa0f7bf52166b147fd16be2be4662";

    #[test]
    fn watch_fixture_matches_the_format() {
        let rendered = pretty(&result(Watch {
            path: format!("{HOME}/models"),
            added: true,
            config: format!("{HOME}/.modeld/config.toml"),
        }));
        assert_fixture("watch.json", &rendered);
        assert_round_trips::<Watch>(&rendered);
    }

    fn space(on_disk: u64, saved: u64, pending: u64, store_only: u64, logical: u64) -> Space {
        Space {
            on_disk,
            saved,
            pending,
            store_only,
            logical,
        }
    }

    fn sample_ls() -> Ls {
        let qwen = Model {
            digest: QWEN.to_string(),
            name: "Qwen3.5-0.8B".to_string(),
            kind: Some("model".to_string()),
            format: Some("GGUF".to_string()),
            quant: Some("Q4_K_M".to_string()),
            params: Some("0.8B".to_string()),
            architecture: Some("qwen3".to_string()),
            size: 532_517_120,
            stored: Stored {
                path: format!(
                    "{HOME}/.modeld/blobs/sha256-04b1c301231dd422b8860db31311ab2721511346a32cb1e079c4c4e5f1fe4696"
                ),
                present: true,
            },
            references: vec![
                Reference {
                    path: format!(
                        "{HOME}/.lmstudio/models/lmstudio-community/Qwen3.5-0.8B-GGUF/Qwen3.5-0.8B-Q4_K_M.gguf"
                    ),
                    provider: "lmstudio".to_string(),
                    label: Some(
                        "lmstudio-community/Qwen3.5-0.8B-GGUF/Qwen3.5-0.8B-Q4_K_M.gguf".to_string(),
                    ),
                    source: Some("lmstudio-community/Qwen3.5-0.8B-GGUF".to_string()),
                    tags: vec![],
                    state: ReferenceState::Clone,
                },
                Reference {
                    path: format!("{HOME}/code/demo/fixtures/models/qwen.gguf"),
                    provider: "manual".to_string(),
                    label: Some("code/demo/fixtures/models/qwen.gguf".to_string()),
                    source: Some("code/demo/fixtures/models".to_string()),
                    tags: vec![],
                    state: ReferenceState::Clone,
                },
                Reference {
                    path: format!("{HOME}/Downloads/models/qwen-copy.gguf"),
                    provider: "manual".to_string(),
                    label: Some("Downloads/models/qwen-copy.gguf".to_string()),
                    source: Some("Downloads/models".to_string()),
                    tags: vec![],
                    state: ReferenceState::Copy,
                },
            ],
            space: space(1_065_034_240, 532_517_120, 532_517_120, 0, 1_597_551_360),
        };
        let minilm = Model {
            digest: MINILM.to_string(),
            name: "all-MiniLM-L6-v2".to_string(),
            kind: Some("model".to_string()),
            format: Some("GGUF".to_string()),
            quant: Some("F16".to_string()),
            params: Some("22M".to_string()),
            architecture: Some("bert".to_string()),
            size: 45_949_216,
            stored: Stored {
                path: format!(
                    "{HOME}/.modeld/blobs/sha256-797b70c4edf85907fe0a49eb85811256f65fa0f7bf52166b147fd16be2be4662"
                ),
                present: true,
            },
            references: vec![Reference {
                path: format!(
                    "{HOME}/.ollama/models/blobs/sha256-797b70c4edf85907fe0a49eb85811256f65fa0f7bf52166b147fd16be2be4662"
                ),
                provider: "ollama".to_string(),
                label: Some("ollama/all-minilm:22m".to_string()),
                source: Some("all-minilm:22m, all-minilm:latest".to_string()),
                tags: vec![
                    "all-minilm:22m".to_string(),
                    "all-minilm:latest".to_string(),
                ],
                state: ReferenceState::Missing,
            }],
            space: space(45_949_216, 0, 0, 45_949_216, 0),
        };
        Ls {
            totals: space(
                1_110_983_456,
                532_517_120,
                532_517_120,
                45_949_216,
                1_597_551_360,
            ),
            models: vec![qwen, minilm],
        }
    }

    fn sample_doctor() -> Doctor {
        Doctor {
            store: format!("{HOME}/.modeld"),
            roots: vec![
                Root {
                    provider: "ollama".to_string(),
                    path: format!("{HOME}/.ollama/models"),
                    excluded: vec![],
                },
                Root {
                    provider: "huggingface".to_string(),
                    path: format!("{HOME}/.cache/huggingface/hub"),
                    excluded: vec![format!("{HOME}/.cache/huggingface/xet")],
                },
                Root {
                    provider: "manual".to_string(),
                    path: format!("{HOME}/code/demo/fixtures/models"),
                    excluded: vec![],
                },
            ],
            unreadable: vec![],
        }
    }

    fn sample_status() -> DaemonStatus {
        DaemonStatus {
            state: AgentState::Running,
            pid: Some(4242),
            binary: format!("{HOME}/.modeld/bin/modeld"),
            binary_is_current: Some(true),
            plist: format!("{HOME}/Library/LaunchAgents/dev.modeld.daemon.plist"),
            log: format!("{HOME}/.modeld/daemon.log"),
            dry_run: false,
            environment: BTreeMap::from([
                ("HF_HOME".to_string(), "/Volumes/Models/hf".to_string()),
                ("HOME".to_string(), HOME.to_string()),
            ]),
            last_pass: Some(LastPass {
                at: "2026-09-23T21:45:21Z".to_string(),
                summary: "pass: 1 clone(s) made (532.5 MB freed)".to_string(),
                failed: false,
            }),
            recent_log: vec![
                "2026-09-23T21:45:03Z hashing /Users/me/Downloads/models/qwen-copy.gguf (532.5 MB)"
                    .to_string(),
                "2026-09-23T21:45:21Z cloned /Users/me/Downloads/models/qwen-copy.gguf".to_string(),
                "2026-09-23T21:45:21Z pass: 1 clone(s) made (532.5 MB freed)".to_string(),
            ],
        }
    }

    fn sample_pass_lines() -> String {
        let copy = format!("{HOME}/Downloads/models/qwen-copy.gguf");
        let events = [
            Event::WaitingForLock,
            Event::Locked,
            Event::Hashing {
                path: copy.clone(),
                size: 532_517_120,
            },
            Event::Cloning {
                path: copy.clone(),
                size: 532_517_120,
            },
        ];
        let mut lines: Vec<String> = events
            .into_iter()
            .map(|event| {
                serde_json::to_string(&Line::<()>::Event {
                    schema: SCHEMA_VERSION,
                    event,
                })
                .expect("serialize")
            })
            .collect();
        let pass = Pass {
            dry_run: false,
            imported: 0,
            synced: 3,
            pruned: Some(0),
            complete: false,
            planned: vec![Planned {
                path: copy.clone(),
                digest: QWEN.to_string(),
                size: 532_517_120,
            }],
            cloned: vec![copy],
            freed: 532_517_120,
            refused: vec![],
            deferred: vec![Deferred {
                path: format!("{HOME}/Downloads/models/new.gguf"),
                settles_in_secs: 289,
            }],
            skipped: vec![],
            warnings: vec![],
            summary: "pass: 1 clone(s) made (532.5 MB freed), 1 settling".to_string(),
        };
        lines.push(serde_json::to_string(&result(pass)).expect("serialize"));
        lines.join("\n") + "\n"
    }

    fn sample_error() -> Line<()> {
        Line::Error {
            schema: SCHEMA_VERSION,
            error: Failure {
                code: ErrorCode::StoreUnavailable,
                message: "cannot open store: store I/O error: Permission denied (os error 13)"
                    .to_string(),
            },
        }
    }

    #[test]
    fn ls_fixture_matches_the_format() {
        let rendered = pretty(&result(sample_ls()));
        assert_fixture("ls.json", &rendered);
        assert_round_trips::<Ls>(&rendered);
    }

    #[test]
    fn doctor_fixture_matches_the_format() {
        let rendered = pretty(&result(sample_doctor()));
        assert_fixture("doctor.json", &rendered);
        assert_round_trips::<Doctor>(&rendered);
    }

    #[test]
    fn daemon_status_fixture_matches_the_format() {
        let rendered = pretty(&result(sample_status()));
        assert_fixture("daemon-status.json", &rendered);
        assert_round_trips::<DaemonStatus>(&rendered);
    }

    #[test]
    fn pass_fixture_matches_the_format() {
        let rendered = sample_pass_lines();
        assert_fixture("pass.jsonl", &rendered);
        for line in rendered.lines() {
            assert_round_trips::<Pass>(line);
        }
    }

    #[test]
    fn discover_fixture_matches_the_format() {
        let games = format!("{HOME}/games");
        let file = |path: String, likely_copy: bool, reclaimable: u64| DiscoveredFile {
            path,
            size: 3_427_863_872,
            likely_copy,
            reclaimable,
        };
        let discover = Discover {
            suggestions: vec![
                FolderSuggestion {
                    pattern: Some("~/games/*/Assets".to_string()),
                    folders: vec![
                        format!("{games}/orc/Assets/StreamingAssets"),
                        format!("{games}/tavern/Assets"),
                    ],
                    size: 3 * 3_427_863_872,
                    reclaimable: 3_427_863_872,
                    files: vec![
                        file(
                            format!("{games}/orc/Assets/StreamingAssets/npc.gguf"),
                            false,
                            0,
                        ),
                        file(
                            format!("{games}/tavern/Assets/npc.gguf"),
                            true,
                            3_427_863_872,
                        ),
                        file(format!("{games}/tavern/Assets/npc-clone.gguf"), true, 0),
                    ],
                },
                FolderSuggestion {
                    pattern: None,
                    folders: vec![format!("{HOME}/models/whisper")],
                    size: 262_300_000,
                    reclaimable: 0,
                    files: vec![DiscoveredFile {
                        path: format!("{HOME}/models/whisper/ggml-large-v3-turbo.bin"),
                        size: 262_300_000,
                        likely_copy: false,
                        reclaimable: 0,
                    }],
                },
            ],
            loose: vec![DiscoveredFile {
                path: format!("{HOME}/stray.gguf"),
                size: 120_000_000,
                likely_copy: false,
                reclaimable: 0,
            }],
            min_size: 100_000_000,
            folders_read: 629_226,
            unreadable: 3,
            cloud_only: 0,
        };
        let lines = [
            serde_json::to_string(&Line::<()>::Event {
                schema: SCHEMA_VERSION,
                event: Event::Discovering { folders: 4096 },
            }),
            serde_json::to_string(&result(discover)),
        ];
        let rendered: String = lines
            .into_iter()
            .map(|line| line.expect("serialize") + "\n")
            .collect();
        assert_fixture("discover.jsonl", &rendered);
        for line in rendered.lines() {
            assert_round_trips::<Discover>(line);
        }
    }

    #[test]
    fn error_fixture_matches_the_format() {
        let rendered = pretty(&sample_error());
        assert_fixture("error.json", &rendered);
        assert_round_trips::<()>(&rendered);
    }

    #[test]
    fn daemon_install_fixture_matches_the_format() {
        let install = DaemonInstall {
            binary: format!("{HOME}/.modeld/bin/modeld"),
            plist: format!("{HOME}/Library/LaunchAgents/dev.modeld.daemon.plist"),
            log: format!("{HOME}/.modeld/daemon.log"),
            status: sample_status(),
        };
        let rendered = pretty(&result(install));
        assert_fixture("daemon-install.json", &rendered);
        assert_round_trips::<DaemonInstall>(&rendered);
    }

    #[test]
    fn daemon_uninstall_fixture_matches_the_format() {
        let rendered = pretty(&result(DaemonUninstall {
            was_installed: true,
        }));
        assert_fixture("daemon-uninstall.json", &rendered);
        assert_round_trips::<DaemonUninstall>(&rendered);
    }

    fn sample_removal_plan() -> RemovalPlan {
        let lmstudio = format!(
            "{HOME}/.lmstudio/models/lmstudio-community/Qwen3.5-0.8B-GGUF/Qwen3.5-0.8B-Q4_K_M.gguf"
        );
        let blob = format!(
            "{HOME}/.modeld/blobs/sha256-04b1c301231dd422b8860db31311ab2721511346a32cb1e079c4c4e5f1fe4696"
        );
        RemovalPlan {
            plan_id: "9f86d081884c7d65".to_string(),
            models: vec![RemovalModel {
                digest: QWEN.to_string(),
                name: "Qwen3.5-0.8B".to_string(),
                size: 532_517_120,
                everywhere: true,
            }],
            steps: vec![
                RemovalStep {
                    action: StepAction::OllamaDelete,
                    provider: "ollama".to_string(),
                    path: format!(
                        "{HOME}/.ollama/models/manifests/registry.ollama.ai/library/qwen3.5/0.8b"
                    ),
                    tag: Some("qwen3.5:0.8b".to_string()),
                    digest: QWEN.to_string(),
                },
                RemovalStep {
                    action: StepAction::TrashFolder,
                    provider: "lmstudio".to_string(),
                    path: format!("{HOME}/.lmstudio/models/lmstudio-community/Qwen3.5-0.8B-GGUF"),
                    tag: None,
                    digest: QWEN.to_string(),
                },
                RemovalStep {
                    action: StepAction::Trash,
                    provider: "manual".to_string(),
                    path: format!("{HOME}/code/demo/fixtures/models/qwen.gguf"),
                    tag: None,
                    digest: QWEN.to_string(),
                },
                RemovalStep {
                    action: StepAction::RemoveStoredCopy,
                    provider: "store".to_string(),
                    path: blob.clone(),
                    tag: None,
                    digest: QWEN.to_string(),
                },
            ],
            refusals: vec![RemovalRefusal {
                code: crate::remove::RefusalCode::InUse,
                path: lmstudio.clone(),
                message: "open in LM Studio".to_string(),
                processes: vec!["LM Studio".to_string()],
            }],
            notes: vec![RemovalNote {
                code: crate::remove::NoteCode::GitRepository,
                path: format!("{HOME}/code/demo/fixtures/models/qwen.gguf"),
                message: format!(
                    "inside the git repository {HOME}/code/demo; if git tracks this file, it shows as deleted"
                ),
                paths: vec![format!("{HOME}/code/demo")],
            }],
            collateral: vec![RemovalCollateral {
                path: format!(
                    "{HOME}/.lmstudio/models/lmstudio-community/Qwen3.5-0.8B-GGUF/README.md"
                ),
                size: 4096,
                reason: "rest of the LM Studio folder lmstudio-community/Qwen3.5-0.8B-GGUF"
                    .to_string(),
            }],
            frees: Frees {
                now: 0,
                after_trash: 532_521_216,
                pinned: 0,
            },
        }
    }

    #[test]
    fn rm_plan_fixture_matches_the_format() {
        let rendered = pretty(&result(Rm {
            status: crate::rm::Status::Planned,
            plan: sample_removal_plan(),
            outcome: None,
        }));
        assert_fixture("rm-plan.json", &rendered);
        assert_round_trips::<Rm>(&rendered);
    }

    #[test]
    fn rm_removed_fixture_matches_the_format() {
        let mut plan = sample_removal_plan();
        plan.refusals.clear();
        let rendered = pretty(&result(Rm {
            status: crate::rm::Status::Removed,
            plan,
            outcome: Some(RemovalOutcome {
                completed: vec![0, 1, 2, 3],
                failure: None,
                stopped: false,
                warnings: vec![],
            }),
        }));
        assert_fixture("rm-removed.json", &rendered);
        assert_round_trips::<Rm>(&rendered);
    }

    #[test]
    fn every_line_names_its_type_and_schema() {
        let line = serde_json::to_value(Line::<()>::Event {
            schema: SCHEMA_VERSION,
            event: Event::Locked,
        })
        .expect("encode");

        assert_eq!(line["type"], "event");
        assert_eq!(line["schema"], 1);
        assert_eq!(line["event"]["kind"], "locked");
    }
}
