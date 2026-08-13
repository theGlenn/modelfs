use clap::{Parser, Subcommand};
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
    /// Consolidate byte-identical duplicates into APFS clones (journaled, reversible)
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
        Command::Dedupe { dry_run, min_size } => dedupe(dry_run, min_size),
        Command::Restore => restore(),
    }
}

fn doctor() {
    let roots = modeld_providers::detect_all();
    if roots.is_empty() {
        println!("No known model providers detected.");
        return;
    }
    for provider in roots {
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

fn dedupe(dry_run: bool, min_size: u64) {
    let Some(outcome) = scan_and_hash(min_size) else {
        return;
    };
    let groups = modeld_core::dedup::duplicate_groups(&outcome.artifacts);
    let replacements = plan::build(&outcome.artifacts, &groups);
    if replacements.is_empty() {
        println!("No consolidatable duplicates found.");
        return;
    }

    print!("{}", report::render_plan(&replacements, dry_run));
    if dry_run {
        return;
    }

    let journal = match modeld_core::consolidate::Journal::open(journal_path()) {
        Ok(journal) => journal,
        Err(error) => {
            eprintln!("cannot open journal: {error}");
            std::process::exit(1);
        }
    };
    let result = modeld_core::consolidate::consolidate(&replacements, &journal);
    print!("{}", report::render_consolidation(&result, "Freed"));
}

fn restore() {
    let journal = match modeld_core::consolidate::Journal::open(journal_path()) {
        Ok(journal) => journal,
        Err(error) => {
            eprintln!("cannot open journal: {error}");
            std::process::exit(1);
        }
    };
    match modeld_core::consolidate::restore(&journal) {
        Ok(result) => print!("{}", report::render_consolidation(&result, "Re-expanded")),
        Err(error) => {
            eprintln!("restore failed: {error}");
            std::process::exit(1);
        }
    }
}

fn scan_and_hash(min_size: u64) -> Option<modeld_providers::scan::ScanOutcome> {
    let roots = modeld_providers::detect_all();
    if roots.is_empty() {
        println!("No known model providers detected — nothing to do.");
        return None;
    }
    let mut outcome = modeld_providers::scan::scan(&roots, min_size);
    modeld_providers::scan::hash_for_dedup(&mut outcome, |path, size| {
        eprintln!("hashing {} ({})", path.display(), report::human_bytes(size));
    });
    Some(outcome)
}

fn journal_path() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("/"), PathBuf::from)
        .join(".modeld/journal.jsonl")
}
