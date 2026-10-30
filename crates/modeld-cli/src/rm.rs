//! `modeld rm`: plan, confirm, and run a removal (see [`crate::remove`]).
//!
//! `--dry-run` prints the plan and changes nothing. Removing takes the store
//! lock (waiting for a daemon pass if need be) and plans again under it.
//! With `--expect-plan ID`, it runs only if the new plan has that id, and
//! otherwise exits 3 with the new plan. Without it, a terminal asks for
//! confirmation (`--yes` skips the question); `--json` requires one or the
//! other. A plan with refusals never runs.
//!
//! Exit codes: 0 when planned or removed, 3 when the plan changed, 1 when
//! refused, failed, or stopped. With `--json` the result line comes first
//! in every one of these cases. Like `pass`, `rm` keeps `SIGPIPE` ignored
//! and drops write errors: a reader quitting never stops it between steps.

use crate::json::{self, ErrorCode, Event, EventWriter};
use crate::remove::{self, OllamaApi, Outcome, Plan, Planner, Scope};
use crate::{Output, fail};
use modeld_core::Digest;
use modeld_store::{Store, StoreLock};
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

/// How often a removal waiting for the store lock checks for a stop.
const LOCK_POLL: Duration = Duration::from_millis(250);

/// Flags beyond what to remove.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub dry_run: bool,
    pub expect_plan: Option<String>,
    pub yes: bool,
}

/// How the command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A dry run: the plan, nothing changed.
    Planned,
    /// The plan under the lock differs from the expected one; nothing changed.
    PlanChanged,
    /// Something blocks the removal; nothing changed.
    Refused,
    /// Every step ran.
    Removed,
    /// A step failed; the steps before it ran.
    Failed,
    /// A stop signal ended the removal between steps.
    Stopped,
}

impl Status {
    fn exit_code(self) -> i32 {
        match self {
            Self::Planned | Self::Removed => 0,
            Self::PlanChanged => 3,
            Self::Refused | Self::Failed | Self::Stopped => 1,
        }
    }
}

/// Runs `modeld rm`; exits with the status's code.
pub fn run(
    store_root: &Path,
    digests: &[Digest],
    scope: &Scope,
    options: &Options,
    output: Output,
) {
    let session = Session::open(store_root, digests, scope, output);
    if options.dry_run {
        let plan = session.plan(false, &mut |_, _| {});
        finish(output, Status::Planned, &plan, None, false);
    }
    let expected = match (&options.expect_plan, options.yes, output) {
        (Some(id), _, _) => Some(id.clone()),
        (None, true, _) => None,
        (None, false, Output::Json) => fail(
            output,
            ErrorCode::ConfirmationRequired,
            "pass --expect-plan with the id of a dry run, or --yes",
        ),
        (None, false, Output::Text) => Some(confirm(&session.plan(false, &mut |_, _| {}))),
    };
    session.remove(expected.as_deref());
}

/// What one `rm` works with: the store, the scanned folders, and Ollama.
struct Session<'a> {
    store_root: &'a Path,
    store: Store,
    roots: Vec<modeld_providers::ProviderRoot>,
    api: OllamaApi,
    digests: &'a [Digest],
    scope: &'a Scope,
    output: Output,
}

impl<'a> Session<'a> {
    fn open(store_root: &'a Path, digests: &'a [Digest], scope: &'a Scope, output: Output) -> Self {
        let store = Store::open(store_root.to_path_buf()).unwrap_or_else(|error| {
            fail(
                output,
                ErrorCode::StoreUnavailable,
                format!("cannot open store: {error}"),
            )
        });
        Self {
            store_root,
            store,
            roots: modeld_providers::detect_all().roots,
            api: OllamaApi::from_env(),
            digests,
            scope,
            output,
        }
    }

    /// Plans the removal; `verify` hashes files whose digest is not cached.
    fn plan(&self, verify: bool, on_hashing: &mut dyn FnMut(&Path, u64)) -> Plan {
        let planner = Planner {
            store: &self.store,
            roots: &self.roots,
            ollama: &self.api,
            verify,
        };
        planner
            .plan(self.digests, self.scope, on_hashing)
            .unwrap_or_else(|error| {
                fail(
                    self.output,
                    ErrorCode::RegistryUnreadable,
                    format!("cannot read registry: {error}"),
                )
            })
    }

    /// Takes the lock, plans again, and runs the plan if it may; exits.
    fn remove(&self, expected: Option<&str>) -> ! {
        let output = self.output;
        let stop = crate::daemon::stop_flag().unwrap_or_else(|error| {
            fail(
                output,
                ErrorCode::RemoveFailed,
                format!("cannot handle stop signals: {error}"),
            )
        });
        let events = EventWriter::start();
        let send = |event: Event| match output {
            Output::Json => events.send(event),
            Output::Text => print_event(&event),
        };
        let Some(lock) = lock_store(self.store_root, &stop, &send) else {
            events.finish();
            fail(
                output,
                ErrorCode::Cancelled,
                "stopped while waiting for another modeld process",
            )
        };
        send(Event::Locked);
        let plan = self.plan(true, &mut |path, size| {
            send(Event::Hashing {
                path: path.to_string_lossy().into_owned(),
                size,
            });
        });
        let (status, outcome) = if expected.is_some_and(|id| id != plan.id) {
            (Status::PlanChanged, None)
        } else if !plan.refusals.is_empty() {
            (Status::Refused, None)
        } else {
            self.execute(&plan, &stop, &send)
        };
        drop(lock);
        events.finish();
        let shown = expected.is_some() && output == Output::Text;
        finish(output, status, &plan, outcome.as_ref(), shown)
    }

    fn execute(
        &self,
        plan: &Plan,
        stop: &std::sync::atomic::AtomicBool,
        send: &impl Fn(Event),
    ) -> (Status, Option<Outcome>) {
        let executor = remove::Executor {
            store: &self.store,
            ollama: &self.api,
            trash: remove::trash,
        };
        let outcome = executor.run(plan, stop, |step| {
            send(Event::Removing {
                path: step.path.to_string_lossy().into_owned(),
            });
        });
        let _ = crate::generation::bump(self.store_root);
        let status = if outcome.failure.is_some() {
            Status::Failed
        } else if outcome.stopped {
            Status::Stopped
        } else {
            Status::Removed
        };
        (status, Some(outcome))
    }
}

/// Shows the plan and asks; returns its id once confirmed, else exits.
fn confirm(plan: &Plan) -> String {
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "{}", remove::text::render_plan(plan));
    if !plan.refusals.is_empty() {
        let _ = writeln!(stdout, "Nothing removed: fix what is blocked first.");
        std::process::exit(Status::Refused.exit_code());
    }
    if !std::io::stdin().is_terminal() {
        let _ = writeln!(
            stdout,
            "Nothing removed: add --yes to remove without a question."
        );
        std::process::exit(1);
    }
    let _ = write!(stdout, "Remove? [y/N] ");
    let _ = stdout.flush();
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        let _ = writeln!(stdout, "Nothing removed.");
        std::process::exit(1);
    }
    plan.id.clone()
}

/// Takes the store lock, waiting (and saying so) while another holds it.
fn lock_store(
    store_root: &Path,
    stop: &std::sync::atomic::AtomicBool,
    send: &impl Fn(Event),
) -> Option<StoreLock> {
    if let Ok(Some(lock)) = StoreLock::try_acquire(store_root) {
        return Some(lock);
    }
    send(Event::WaitingForLock);
    StoreLock::acquire_unless(store_root, LOCK_POLL, || {
        stop.load(std::sync::atomic::Ordering::Relaxed)
    })
    .ok()
    .flatten()
}

/// Reports the result and exits with the status's code.
fn finish(
    output: Output,
    status: Status,
    plan: &Plan,
    outcome: Option<&Outcome>,
    plan_shown: bool,
) -> ! {
    match output {
        Output::Json => json::emit_result(json::Rm::new(status, plan, outcome)),
        Output::Text => {
            let mut text = String::new();
            if status == Status::PlanChanged {
                text.push_str("The plan changed while waiting; nothing was removed. New plan:\n");
            }
            if !plan_shown || status == Status::PlanChanged {
                text.push_str(&remove::text::render_plan(plan));
            }
            if status == Status::Refused {
                text.push_str("Nothing removed: fix what is blocked first.\n");
            }
            if let Some(outcome) = outcome {
                text.push_str(&remove::text::render_outcome(plan, outcome));
            }
            let _ = write!(std::io::stdout(), "{text}");
        }
    }
    std::process::exit(status.exit_code())
}

/// Text-mode progress on stderr.
fn print_event(event: &Event) {
    let mut stderr = std::io::stderr();
    let _ = match event {
        Event::WaitingForLock => writeln!(
            stderr,
            "waiting for another modeld process (daemon pass?) to finish…"
        ),
        Event::Hashing { path, size } => writeln!(
            stderr,
            "hashing {path} ({})",
            crate::report::human_bytes(*size)
        ),
        Event::Removing { path } => writeln!(stderr, "removing {path}"),
        Event::Locked | Event::Cloning { .. } | Event::Discovering { .. } => Ok(()),
    };
}
