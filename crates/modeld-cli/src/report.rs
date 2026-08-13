//! Renders scan results for terminal output.

use modeld_core::dedup::{DuplicateGroup, Summary};
use modeld_providers::scan::ScanOutcome;
use std::fmt::Write;

/// Formats a byte count with a decimal unit, e.g. `2.6 GB`.
#[expect(
    clippy::cast_precision_loss,
    reason = "display-only rounding to one decimal; exact bytes never shown via f64"
)]
pub fn human_bytes(bytes: u64) -> String {
    // Decimal units match what Ollama, LM Studio, and Finder show users on macOS.
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Renders the full scan report.
pub fn render(outcome: &ScanOutcome, groups: &[DuplicateGroup], summary: Summary) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Found {} model artifacts", summary.artifact_count);
    let _ = writeln!(
        out,
        "Total:             {}",
        human_bytes(summary.total_bytes)
    );
    let _ = writeln!(
        out,
        "Unique:            {}",
        human_bytes(summary.total_bytes - summary.reclaimable_bytes)
    );
    let _ = writeln!(
        out,
        "Potential savings: {}",
        human_bytes(summary.reclaimable_bytes)
    );

    if !groups.is_empty() {
        let _ = writeln!(out, "\nDuplicates:");
        for group in groups {
            render_group(&mut out, outcome, group);
        }
    }

    if !outcome.skipped.is_empty() {
        let _ = writeln!(
            out,
            "\nSkipped {} paths (unreadable or in-flight):",
            outcome.skipped.len()
        );
        for skip in &outcome.skipped {
            let _ = writeln!(out, "  {}  ({})", skip.path.display(), skip.reason);
        }
    }
    out
}

fn render_group(out: &mut String, outcome: &ScanOutcome, group: &DuplicateGroup) {
    let title = group
        .members
        .iter()
        .find_map(|&i| outcome.artifacts[i].label.clone())
        .unwrap_or_else(|| group.digest.to_string());
    let _ = writeln!(out, "\n{title}");
    for &index in &group.members {
        let artifact = &outcome.artifacts[index];
        let _ = writeln!(
            out,
            "  {}  {}",
            artifact.path.display(),
            human_bytes(artifact.size)
        );
    }
    let _ = writeln!(out, "  {}", group.digest);
    let _ = writeln!(
        out,
        "  Potential saving: {}",
        human_bytes(group.reclaimable)
    );
}

/// Renders the dedupe plan: what will (or would) be replaced by clones.
pub fn render_plan(
    replacements: &[modeld_core::consolidate::Replacement],
    dry_run: bool,
) -> String {
    let mut out = String::new();
    let verb = if dry_run {
        "Would replace"
    } else {
        "Replacing"
    };
    let total: u64 = replacements.iter().map(|r| r.size).sum();
    for replacement in replacements {
        let _ = writeln!(
            out,
            "{verb} {}\n     with clone of {}  ({})",
            replacement.victim.display(),
            replacement.canonical.display(),
            human_bytes(replacement.size)
        );
    }
    let _ = writeln!(
        out,
        "{} replacement(s), {} reclaimable",
        replacements.len(),
        human_bytes(total)
    );
    out
}

/// Renders the result of a consolidate or restore run.
pub fn render_consolidation(report: &modeld_core::consolidate::Report, verb: &str) -> String {
    let mut out = String::new();
    for path in &report.completed {
        let _ = writeln!(out, "ok  {}", path.display());
    }
    for refusal in &report.refused {
        let _ = writeln!(
            out,
            "SKIP {}  ({})",
            refusal.victim.display(),
            refusal.reason
        );
    }
    let _ = writeln!(
        out,
        "{verb} {} across {} file(s); {} skipped",
        human_bytes(report.bytes_affected),
        report.completed.len(),
        report.refused.len()
    );
    out
}

/// Renders the `ls` table: stored artifacts and which providers use them.
pub fn render_ls(
    artifacts: &[modeld_store::StoredArtifact],
    totals: modeld_store::Totals,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<52} {:<12} {:>9}  USED BY",
        "MODEL", "FORMAT", "SIZE"
    );
    for artifact in artifacts {
        let label = artifact
            .references
            .iter()
            .find_map(|r| r.label.clone())
            .unwrap_or_else(|| short_digest(&artifact.digest.to_string()));
        let mut providers: Vec<&str> = artifact
            .references
            .iter()
            .map(|r| r.provider.as_str())
            .collect();
        providers.sort_unstable();
        providers.dedup();
        let _ = writeln!(
            out,
            "{:<52} {:<12} {:>9}  {}",
            truncate(&label, 52),
            artifact.format.as_deref().unwrap_or("-"),
            human_bytes(artifact.size),
            if providers.is_empty() {
                "-".to_string()
            } else {
                providers.join(", ")
            }
        );
    }
    let _ = writeln!(out);
    out.push_str(&render_totals(totals));
    out
}

/// Renders the storage accounting footer.
pub fn render_totals(totals: modeld_store::Totals) -> String {
    let saved = totals.logical_bytes.saturating_sub(totals.physical_bytes);
    format!(
        "Physical usage:  {}\nLogical usage:   {}\nDeduplicated:    {}\n",
        human_bytes(totals.physical_bytes),
        human_bytes(totals.logical_bytes),
        human_bytes(saved)
    )
}

/// Renders `where` results: canonical blob plus every reference.
pub fn render_where(
    matches: &[modeld_store::StoredArtifact],
    store: &modeld_store::Store,
) -> String {
    let mut out = String::new();
    for artifact in matches {
        let _ = writeln!(out, "{}", artifact.digest);
        let _ = writeln!(
            out,
            "  Canonical: {}",
            store.blob_path(&artifact.digest).display()
        );
        let _ = writeln!(out, "  Referenced by:");
        for reference in &artifact.references {
            let _ = writeln!(
                out,
                "    {:<12} {}",
                reference.provider,
                reference.path.display()
            );
        }
    }
    out
}

fn short_digest(digest: &str) -> String {
    digest.chars().take(19).collect()
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let head: String = text.chars().take(max - 1).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_uses_expected_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2_600_000_000), "2.6 GB");
        assert_eq!(human_bytes(45_949_216), "45.9 MB");
    }
}
