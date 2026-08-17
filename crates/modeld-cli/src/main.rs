use clap::{Parser, Subcommand};
use modeld_core::{Artifact, Digest};
use modeld_providers::scan::ScanOutcome;
use modeld_store::{FileStamp, Store};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

mod plan;
mod report;

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
    let Some(mut outcome) = scan_providers(min_size) else {
        return;
    };
    let store = open_store();
    let stamp = modeld_store::sync_stamp();
    let mut imported = 0usize;
    let mut referenced = 0usize;
    let mut can_prune = outcome.skipped.is_empty();

    for artifact in &mut outcome.artifacts {
        if !ensure_verified_digest(&store, artifact) {
            can_prune = false;
            eprintln!(
                "skip {} (file changed or could not be hashed)",
                artifact.path.display()
            );
            continue;
        }
        let Some(digest) = artifact.digest.clone() else {
            continue;
        };
        if digest.algorithm() != modeld_core::Algorithm::Sha256 {
            continue;
        }
        match store.import_blob(&artifact.path, &digest, artifact.format.as_ref()) {
            Ok(modeld_store::ImportOutcome::Imported) => imported += 1,
            Ok(modeld_store::ImportOutcome::AlreadyPresent) => {}
            Ok(modeld_store::ImportOutcome::NotCloneable) => {
                can_prune = false;
                eprintln!("skip {} (different volume)", artifact.path.display());
                continue;
            }
            Err(error) => {
                can_prune = false;
                eprintln!("skip {} ({error})", artifact.path.display());
                continue;
            }
        }
        let recorded = store.record_reference(
            &digest,
            &artifact.path,
            artifact.provider,
            artifact.label.as_deref(),
            stamp,
        );
        match recorded {
            Ok(()) => referenced += 1,
            Err(error) => {
                can_prune = false;
                eprintln!("skip ref {} ({error})", artifact.path.display());
            }
        }
    }

    let pruned = if can_prune {
        match store.prune_references_before(stamp) {
            Ok(pruned) => pruned,
            Err(error) => {
                eprintln!("warning: stale references were not pruned ({error})");
                0
            }
        }
    } else {
        eprintln!("warning: incomplete sync; stale references were not pruned");
        0
    };
    println!(
        "Synced: {imported} new blob(s), {referenced} reference(s), {pruned} stale reference(s) pruned"
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
    let result = modeld_core::consolidate::consolidate(&replacements, &journal);
    let digests_by_path: HashMap<_, _> = replacements
        .iter()
        .map(|replacement| (&replacement.victim, &replacement.digest))
        .collect();
    for path in &result.completed {
        if let Some(digest) = digests_by_path.get(path)
            && let Err(error) = store.record_shared_path(digest, path)
        {
            eprintln!(
                "warning: could not remember clone state for {} ({error})",
                path.display()
            );
        }
    }
    print!("{}", report::render_consolidation(&result, "Freed"));
}

fn restore() {
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

/// Verifies (or computes) the artifact's digest, consulting the store's cache.
///
/// Harvested digests are claims; sync trusts only hashes modeld computed itself.
/// Returns false when the file cannot be hashed.
fn ensure_verified_digest(store: &Store, artifact: &mut Artifact) -> bool {
    if artifact.digest_verified {
        return true;
    }
    let Ok(before) = FileStamp::of(&artifact.path) else {
        return false;
    };
    if before.size != artifact.size {
        return false;
    }
    if let Ok(Some(cached)) = store.cached_digest(&artifact.path, before)
        && FileStamp::of(&artifact.path).is_ok_and(|after| after == before)
    {
        artifact.digest = Some(cached);
        artifact.digest_verified = true;
        return true;
    }
    eprintln!(
        "hashing {} ({})",
        artifact.path.display(),
        report::human_bytes(artifact.size)
    );
    let Ok(actual) = Digest::sha256_file(&artifact.path) else {
        return false;
    };
    if !FileStamp::of(&artifact.path).is_ok_and(|after| after == before) {
        return false;
    }
    let _ = store.remember_digest(&artifact.path, before, &actual);
    artifact.digest = Some(actual);
    artifact.digest_verified = true;
    true
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
    modeld_providers::scan::hash_for_dedup(&mut outcome, |path, size| {
        eprintln!("hashing {} ({})", path.display(), report::human_bytes(size));
    });
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
