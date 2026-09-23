use clap::{Parser, Subcommand};
use modeld_core::Digest;
use modeld_providers::scan::ScanOutcome;
use modeld_store::{Store, StoreLock};
use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod consolidation;
mod daemon;
mod events;
mod plan;
mod reconcile;
mod report;
mod schedule;
mod semantics;
mod sync;

#[derive(Parser)]
#[command(name = "modeld", version, about = "Local model storage layer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report detected providers and their scan roots
    Doctor,
    /// Inventory local model artifacts and report exact duplicates
    Scan {
        /// Ignore files smaller than this many bytes
        #[arg(long, default_value_t = modeld_providers::scan::DEFAULT_MIN_SIZE)]
        min_size: u64,
    },
    /// Import every artifact into the canonical store and refresh the registry
    Sync {
        /// Ignore files smaller than this many bytes
        #[arg(long, default_value_t = modeld_providers::scan::DEFAULT_MIN_SIZE)]
        min_size: u64,
    },
    /// List stored artifacts and which providers use them
    Ls,
    /// Show the canonical blob and references for artifacts matching a query
    Where { query: String },
    /// Consolidate byte-identical duplicates into clones of store blobs
    Dedupe {
        /// Show what would be replaced without touching anything
        #[arg(long)]
        dry_run: bool,
        /// Ignore files smaller than this many bytes
        #[arg(long, default_value_t = modeld_providers::scan::DEFAULT_MIN_SIZE)]
        min_size: u64,
    },
    /// Undo journaled replacements by rebuilding independent copies
    Restore,
    /// Delete store blobs no provider references anymore
    Gc {
        /// Show what would be deleted without touching anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Watch model folders; import settled downloads and clone duplicates
    Daemon {
        /// Keep the registry in sync but only log clones, never make them
        #[arg(long)]
        dry_run: bool,
        /// Ignore files smaller than this many bytes
        #[arg(long, default_value_t = modeld_providers::scan::DEFAULT_MIN_SIZE)]
        min_size: u64,
    },
}

fn main() {
    match Cli::parse().command {
        Command::Doctor => doctor(),
        Command::Scan { min_size } => scan(min_size),
        Command::Sync { min_size } => sync(min_size),
        Command::Ls => ls(),
        Command::Where { query } => locate(&query),
        Command::Dedupe { dry_run, min_size } => dedupe(dry_run, min_size),
        Command::Restore => restore(),
        Command::Gc { dry_run } => gc(dry_run),
        Command::Daemon { dry_run, min_size } => daemon(dry_run, min_size),
    }
}

fn doctor() {
    let detection = modeld_providers::detect_all();
    for warning in &detection.warnings {
        eprintln!("warning: {warning}");
    }
    if detection.roots.is_empty() {
        println!("No known model providers detected.");
        return;
    }
    for provider in detection.roots {
        println!(
            "{:<12} {}",
            format!("{:?}", provider.kind),
            provider.root.display()
        );
        for excluded in &provider.excluded {
            println!("{:<12} └─ excluded: {}", "", excluded.display());
        }
    }
}

fn scan(min_size: u64) {
    let Some(outcome) = scan_and_hash(min_size) else {
        return;
    };
    let groups = modeld_core::dedup::duplicate_groups(&outcome.artifacts);
    let summary = modeld_core::dedup::summarize(&outcome.artifacts, &groups);
    print!("{}", report::render(&outcome, &groups, summary));
}

fn sync(min_size: u64) {
    let _lock = lock_store();
    let Some(outcome) = scan_providers(min_size) else {
        return;
    };
    let store = open_store();
    let report = sync::run(&store, outcome, Duration::ZERO, print_hashing);
    for skipped in &report.skipped {
        eprintln!("skip {} ({})", skipped.path.display(), skipped.reason);
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    if !report.is_complete() {
        eprintln!("warning: incomplete sync; stale references were not pruned");
    }
    println!(
        "Synced: {} new blob(s), {} reference(s), {} stale reference(s) pruned",
        report.imported,
        report.synced.len(),
        report.pruned.unwrap_or(0)
    );
    match store.totals() {
        Ok(totals) => print!("{}", report::render_totals(totals)),
        Err(error) => eprintln!("totals unavailable: {error}"),
    }
}

fn ls() {
    let store = open_store();
    match (store.artifacts(), store.totals()) {
        (Ok(artifacts), Ok(totals)) => {
            if artifacts.is_empty() {
                println!("Store is empty — run `modeld sync` first.");
                return;
            }
            print!("{}", report::render_ls(&artifacts, totals));
        }
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("cannot read registry: {error}");
            std::process::exit(1);
        }
    }
}

fn locate(query: &str) {
    let store = open_store();
    match store.find(query) {
        Ok(matches) if matches.is_empty() => {
            println!("No stored artifact matches `{query}` — run `modeld sync` first?");
        }
        Ok(matches) => print!("{}", report::render_where(&matches, &store)),
        Err(error) => {
            eprintln!("cannot read registry: {error}");
            std::process::exit(1);
        }
    }
}

fn dedupe(dry_run: bool, min_size: u64) {
    let _lock = lock_store();
    let Some(outcome) = scan_and_hash(min_size) else {
        return;
    };
    let groups = modeld_core::dedup::duplicate_groups(&outcome.artifacts);
    let store = open_store();
    let mut replacements = Vec::new();

    for group in &groups {
        if group.digest.algorithm() != modeld_core::Algorithm::Sha256 {
            continue;
        }
        let source = &outcome.artifacts[group.members[0]];
        // Verify the import source by hashing before the store adopts its bytes.
        match Digest::sha256_file(&source.path) {
            Ok(actual) if actual == group.digest => {}
            Ok(_) => {
                eprintln!("skip group {} (source drifted)", group.digest);
                continue;
            }
            Err(error) => {
                eprintln!("skip group {} ({error})", group.digest);
                continue;
            }
        }
        let import = if dry_run {
            None
        } else {
            match store.import_blob(&source.path, &group.digest, source.format.as_ref()) {
                Ok(outcome) => Some(outcome),
                Err(error) => {
                    eprintln!("skip group {} ({error})", group.digest);
                    continue;
                }
            }
        };
        if import == Some(modeld_store::ImportOutcome::NotCloneable) {
            eprintln!("skip group {} (different volume)", group.digest);
            continue;
        }
        let blob = store.blob_path(&group.digest);
        let blob_id = std::fs::metadata(&blob)
            .map(|m| modeld_core::FileId {
                device: m.dev(),
                inode: m.ino(),
            })
            .ok()
            .or_else(|| source.file_id.filter(|_| dry_run));
        let mut already_shared = HashSet::new();
        for &index in &group.members {
            let artifact = &outcome.artifacts[index];
            if store
                .path_is_shared(&group.digest, &artifact.path)
                .unwrap_or(false)
            {
                already_shared.insert(artifact.path.clone());
            }
        }
        if dry_run {
            already_shared.insert(source.path.clone());
        }
        replacements.extend(plan::replacements_for_group(
            &outcome.artifacts,
            group,
            &blob,
            blob_id,
            &already_shared,
        ));
    }

    if replacements.is_empty() {
        println!("No consolidatable duplicates found.");
        return;
    }
    print!("{}", report::render_plan(&replacements, dry_run));
    if dry_run {
        return;
    }

    let journal = open_journal();
    let result = consolidation::apply(&store, &journal, &replacements, |warning| {
        eprintln!("warning: {warning}");
    });
    print!("{}", report::render_consolidation(&result, "Freed"));
}

fn restore() {
    let _lock = lock_store();
    let journal = open_journal();
    match modeld_core::consolidate::restore(&journal) {
        Ok(result) => {
            let store = open_store();
            for path in &result.completed {
                if let Err(error) = store.forget_shared_path(path) {
                    eprintln!(
                        "warning: could not clear clone state for {} ({error})",
                        path.display()
                    );
                }
            }
            print!("{}", report::render_consolidation(&result, "Re-expanded"));
        }
        Err(error) => {
            eprintln!("restore failed: {error}");
            std::process::exit(1);
        }
    }
}

/// Deletes store blobs that nothing references: no provider path, no pending
/// journaled swap, no live clone recorded in `shared_paths`.
fn gc(dry_run: bool) {
    let _lock = lock_store();
    let store = open_store();
    let journal = open_journal();
    // A journaled swap needs its canonical blob to restore; without a readable
    // journal we cannot prove any blob is safe to delete.
    let journaled: HashSet<String> = match journal.entries() {
        Ok(entries) => entries
            .iter()
            .map(|entry| entry.digest.to_string())
            .collect(),
        Err(error) => {
            eprintln!("cannot read journal, refusing to gc: {error}");
            std::process::exit(1);
        }
    };
    let artifacts = match store.artifacts() {
        Ok(artifacts) => artifacts,
        Err(error) => {
            eprintln!("cannot read registry: {error}");
            std::process::exit(1);
        }
    };

    let mut removable = Vec::new();
    let mut kept = Vec::new();
    for artifact in artifacts {
        if !artifact.references.is_empty() {
            continue;
        }
        if journaled.contains(&artifact.digest.to_string()) {
            kept.push((artifact, "journaled swap pending restore"));
            continue;
        }
        match store.has_live_shared_path(&artifact.digest) {
            Ok(true) => kept.push((artifact, "still anchors a live clone")),
            Ok(false) => removable.push(artifact),
            Err(error) => {
                eprintln!("skip {} ({error})", artifact.digest);
            }
        }
    }

    print!("{}", report::render_gc_plan(&removable, &kept, dry_run));
    if dry_run || removable.is_empty() {
        return;
    }

    let mut freed = 0u64;
    let mut deleted = 0usize;
    for artifact in &removable {
        match store.remove_artifact(&artifact.digest) {
            Ok(()) => {
                freed += artifact.size;
                deleted += 1;
            }
            Err(error) => eprintln!("keep {} ({error})", artifact.digest),
        }
    }
    println!(
        "Freed {} across {} blob(s)",
        report::human_bytes(freed),
        deleted
    );
}

fn print_hashing(path: &Path, size: u64) {
    eprintln!("hashing {} ({})", path.display(), report::human_bytes(size));
}

fn scan_providers(min_size: u64) -> Option<ScanOutcome> {
    let detection = modeld_providers::detect_all();
    for warning in &detection.warnings {
        eprintln!("warning: {warning}");
    }
    if detection.roots.is_empty() {
        println!("No known model providers detected — nothing to do.");
        return None;
    }
    Some(modeld_providers::scan::scan(&detection.roots, min_size))
}

fn scan_and_hash(min_size: u64) -> Option<ScanOutcome> {
    let mut outcome = scan_providers(min_size)?;
    modeld_providers::scan::hash_for_dedup(&mut outcome, print_hashing);
    Some(outcome)
}

fn open_store() -> Store {
    match Store::open(modeld_home()) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("cannot open store: {error}");
            std::process::exit(1);
        }
    }
}

fn daemon(dry_run: bool, min_size: u64) {
    let options = reconcile::PassOptions {
        settle: modeld_core::consolidate::RECENT_WRITE_WINDOW,
        min_size,
        dry_run,
    };
    if let Err(error) = daemon::run(&modeld_home(), options) {
        eprintln!("daemon failed: {error}");
        std::process::exit(1);
    }
}

/// Serializes this command with the daemon and other mutating commands.
fn lock_store() -> StoreLock {
    let home = modeld_home();
    let lock = match StoreLock::try_acquire(&home) {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => {
            eprintln!("waiting for another modeld process (daemon pass?) to finish…");
            StoreLock::acquire(&home)
        }
        Err(error) => Err(error),
    };
    match lock {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("cannot lock store: {error}");
            std::process::exit(1);
        }
    }
}

fn open_journal() -> modeld_core::consolidate::Journal {
    match modeld_core::consolidate::Journal::open(modeld_home().join("journal.jsonl")) {
        Ok(journal) => journal,
        Err(error) => {
            eprintln!("cannot open journal: {error}");
            std::process::exit(1);
        }
    }
}

fn modeld_home() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("/"), PathBuf::from)
        .join(".modeld")
}
