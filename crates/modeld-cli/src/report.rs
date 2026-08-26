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

/// Renders the `ls` table: models first, model-adjacent assets after.
pub fn render_ls(
    artifacts: &[modeld_store::StoredArtifact],
    totals: modeld_store::Totals,
) -> String {
    let (assets, models): (Vec<_>, Vec<_>) = artifacts.iter().partition(|artifact| {
        artifact
            .semantics
            .as_ref()
            .is_some_and(|semantics| semantics.kind == "asset")
    });

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<44} {:<12} {:<8} {:>9}  USED BY",
        "MODEL", "FORMAT", "QUANT", "SIZE"
    );
    for artifact in &models {
        render_ls_row(&mut out, artifact);
    }
    if !assets.is_empty() {
        let _ = writeln!(out, "\nAssets (tokenizers, vocabularies):");
        for artifact in &assets {
            render_ls_row(&mut out, artifact);
        }
    }
    let _ = writeln!(out);
    out.push_str(&render_totals(totals));
    out
}

/// The most human name we have: header name, then any label, then the digest.
fn display_name(artifact: &modeld_store::StoredArtifact) -> String {
    artifact
        .semantics
        .as_ref()
        .and_then(|s| s.name.clone())
        .or_else(|| artifact.references.iter().find_map(|r| r.label.clone()))
        .unwrap_or_else(|| short_digest(&artifact.digest.to_string()))
}

fn render_ls_row(out: &mut String, artifact: &modeld_store::StoredArtifact) {
    let semantics = artifact.semantics.as_ref();
    let display_name = display_name(artifact);
    let mut providers: Vec<&str> = artifact
        .references
        .iter()
        .map(|r| r.provider.as_str())
        .collect();
    providers.sort_unstable();
    providers.dedup();
    let _ = writeln!(
        out,
        "{:<44} {:<12} {:<8} {:>9}  {}",
        truncate(&display_name, 44),
        artifact.format.as_deref().unwrap_or("-"),
        semantics.and_then(|s| s.quant.as_deref()).unwrap_or("-"),
        human_bytes(artifact.size),
        if providers.is_empty() {
            "-".to_string()
        } else {
            providers.join(", ")
        }
    );
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

/// Renders the gc plan: unreferenced blobs to delete, guarded blobs kept.
pub fn render_gc_plan(
    removable: &[modeld_store::StoredArtifact],
    kept: &[(modeld_store::StoredArtifact, &str)],
    dry_run: bool,
) -> String {
    let mut out = String::new();
    let verb = if dry_run { "Would delete" } else { "Deleting" };
    for artifact in removable {
        let _ = writeln!(
            out,
            "{verb} {}  {}  ({})",
            artifact.digest,
            display_name(artifact),
            human_bytes(artifact.size)
        );
    }
    for (artifact, reason) in kept {
        let _ = writeln!(
            out,
            "keep {}  {}  ({reason})",
            artifact.digest,
            display_name(artifact)
        );
    }
    let total: u64 = removable.iter().map(|artifact| artifact.size).sum();
    if removable.is_empty() {
        let _ = writeln!(out, "Nothing to gc — every blob is referenced.");
    } else {
        let _ = writeln!(
            out,
            "{} unreferenced blob(s), {} reclaimable",
            removable.len(),
            human_bytes(total)
        );
    }
    out
}

/// Renders `where` results: canonical blob plus every reference.
pub fn render_where(
    matches: &[modeld_store::StoredArtifact],
    store: &modeld_store::Store,
) -> String {
    let mut out = String::new();
    for artifact in matches {
        let _ = writeln!(out, "{}", artifact.digest);
        if let Some(info) = describe_semantics(artifact) {
            let _ = writeln!(out, "  Info: {info}");
        }
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

/// One human line from recorded semantics, e.g.
/// `Qwen3.5 0.8B · GGUF · Q4_K_M · 1.7B params · arch qwen3`.
fn describe_semantics(artifact: &modeld_store::StoredArtifact) -> Option<String> {
    let semantics = artifact.semantics.as_ref()?;
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = &semantics.name {
        parts.push(name.clone());
    }
    if semantics.kind == "asset" {
        parts.push("asset".to_string());
    }
    if let Some(format) = &artifact.format {
        parts.push(format.clone());
    }
    if let Some(quant) = &semantics.quant {
        parts.push(quant.clone());
    }
    if let Some(params) = &semantics.params {
        parts.push(format!("{params} params"));
    }
    if let Some(architecture) = &semantics.architecture {
        parts.push(format!("arch {architecture}"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
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

    fn stored(
        seed: u8,
        label: &str,
        semantics: Option<modeld_store::Semantics>,
    ) -> modeld_store::StoredArtifact {
        modeld_store::StoredArtifact {
            digest: modeld_core::Digest::new(modeld_core::Algorithm::Sha256, vec![seed; 32])
                .expect("digest"),
            size: 1000,
            format: Some("GGUF".to_string()),
            references: vec![modeld_store::Reference {
                path: std::path::PathBuf::from("/x"),
                provider: "manual".to_string(),
                label: Some(label.to_string()),
            }],
            semantics,
        }
    }

    #[test]
    fn ls_prefers_header_name_and_splits_assets_out() {
        let model = stored(
            1,
            "some/path/model.gguf",
            Some(modeld_store::Semantics {
                kind: "model".to_string(),
                name: Some("Bonsai 1.7B".to_string()),
                architecture: Some("llama".to_string()),
                quant: Some("Q1_0".to_string()),
                params: Some("1.7B".to_string()),
            }),
        );
        let asset = stored(
            2,
            "Qwen/Qwen3.5-0.8B: tokenizer.json",
            Some(modeld_store::Semantics {
                kind: "asset".to_string(),
                name: None,
                architecture: None,
                quant: None,
                params: None,
            }),
        );
        let rendered = render_ls(&[asset, model], modeld_store::Totals::default());

        let models_at = rendered.find("Bonsai 1.7B").expect("model row");
        let assets_at = rendered.find("tokenizer.json").expect("asset row");
        assert!(rendered.contains("Assets (tokenizers, vocabularies):"));
        assert!(rendered.contains("Q1_0"));
        assert!(models_at < assets_at, "models list before assets");
    }

    #[test]
    fn where_line_reads_as_one_sentence() {
        let model = stored(
            1,
            "x",
            Some(modeld_store::Semantics {
                kind: "model".to_string(),
                name: Some("Bonsai 1.7B".to_string()),
                architecture: Some("llama".to_string()),
                quant: Some("Q1_0".to_string()),
                params: Some("1.7B".to_string()),
            }),
        );
        assert_eq!(
            describe_semantics(&model).as_deref(),
            Some("Bonsai 1.7B · GGUF · Q1_0 · 1.7B params · arch llama")
        );
    }
}
