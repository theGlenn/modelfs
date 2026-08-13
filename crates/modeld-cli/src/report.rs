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
