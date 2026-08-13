use clap::{Parser, Subcommand};

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
}

fn main() {
    match Cli::parse().command {
        Command::Doctor => doctor(),
        Command::Scan { min_size } => scan(min_size),
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
    let roots = modeld_providers::detect_all();
    if roots.is_empty() {
        println!("No known model providers detected — nothing to scan.");
        return;
    }

    let mut outcome = modeld_providers::scan::scan(&roots, min_size);
    modeld_providers::scan::hash_for_dedup(&mut outcome, |path, size| {
        eprintln!("hashing {} ({})", path.display(), report::human_bytes(size));
    });

    let groups = modeld_core::dedup::duplicate_groups(&outcome.artifacts);
    let summary = modeld_core::dedup::summarize(&outcome.artifacts, &groups);
    print!("{}", report::render(&outcome, &groups, summary));
}
