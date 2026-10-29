//! Suggests folders to watch: large model files no scan root covers.
//!
//! Read-only: discovery changes no configuration and no model files, and it
//! takes no store lock. See `modeld_providers::discover` for the walk.

use crate::report::human_bytes;
use modeld_providers::discover::{Candidate, Discovery, Suggestion};
use modeld_store::Store;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Files listed under each suggestion; the rest are counted.
const LISTED_FILES: usize = 5;

pub fn run(min_size: u64) {
    let home = crate::home_dir();
    let home = home.canonicalize().unwrap_or(home);
    let detection = modeld_providers::detect_all();
    for unreadable in &detection.unreadable {
        eprintln!(
            "warning: {} ({}); folders it adds may be suggested again",
            unreadable.path.display(),
            unreadable.reason
        );
    }
    let stored_models = stored_models(&crate::modeld_home());
    eprintln!(
        "Looking through {} for model files of {} or more…",
        tilde(&home, &home),
        human_bytes(min_size)
    );
    let started = Instant::now();
    let discovery =
        modeld_providers::discover::discover(&home, &detection, &stored_models, min_size);
    print!("{}", render(&discovery, &home, min_size, started.elapsed()));
}

/// The store's blob files, to flag copies of them. Never creates a store.
fn stored_models(store: &Path) -> Vec<PathBuf> {
    if !store.join("registry.db").exists() {
        return Vec::new();
    }
    let read = Store::open(store.to_path_buf()).and_then(|store| {
        let artifacts = store.artifacts()?;
        Ok(artifacts
            .iter()
            .map(|artifact| store.blob_path(&artifact.digest))
            .collect())
    });
    read.unwrap_or_else(|error| {
        eprintln!("warning: cannot read stored models ({error}); copies of them are not flagged");
        Vec::new()
    })
}

fn render(discovery: &Discovery, home: &Path, min_size: u64, elapsed: Duration) -> String {
    let mut out = String::new();
    let suggestions = &discovery.suggestions;
    if suggestions.is_empty() {
        let _ = writeln!(
            out,
            "No unwatched folders hold model files of {} or more.",
            human_bytes(min_size)
        );
    } else {
        let size: u64 = suggestions.iter().map(Suggestion::size).sum();
        let reclaimable: u64 = suggestions.iter().map(Suggestion::reclaimable).sum();
        let _ = write!(
            out,
            "{} not watched by ModelFS hold {} of models",
            plural(suggestions.len(), "folder", "folders"),
            human_bytes(size)
        );
        if reclaimable > 0 {
            let _ = write!(
                out,
                "; adding them could free about {}",
                human_bytes(reclaimable)
            );
        }
        let _ = writeln!(out, ".");
    }
    for suggestion in suggestions {
        render_suggestion(&mut out, suggestion, home);
    }
    if !discovery.loose.is_empty() {
        let _ = writeln!(
            out,
            "\nNot suggested: these files sit directly in your home folder or a project \
             folder, too broad to watch. Move them into a folder of their own to add them."
        );
        for file in &discovery.loose {
            let _ = writeln!(
                out,
                "  {:>8}  {}",
                human_bytes(file.size),
                tilde(&file.path, home)
            );
        }
    }
    let _ = write!(
        out,
        "\nLooked through {} in {}",
        plural(discovery.folders_read, "folder", "folders"),
        duration(elapsed)
    );
    if discovery.unreadable > 0 {
        let _ = write!(
            out,
            "; {} could not be read",
            plural(discovery.unreadable, "item", "items")
        );
    }
    if discovery.cloud_only > 0 {
        let _ = write!(
            out,
            "; left {} stored only in the cloud alone",
            plural(discovery.cloud_only, "item", "items")
        );
    }
    let _ = writeln!(out, ".");
    if suggestions
        .iter()
        .flat_map(|suggestion| &suggestion.files)
        .any(|file| file.likely_copy)
    {
        let _ = writeln!(
            out,
            "A likely copy matches the size and sampled bytes of a stored model or another \
             file listed; one already shared is a clone and frees nothing. Passes compare \
             full contents before they change anything."
        );
    }
    out
}

fn render_suggestion(out: &mut String, suggestion: &Suggestion, home: &Path) {
    let base = common_folder(&suggestion.folders);
    let _ = write!(
        out,
        "\n{}",
        suggestion
            .pattern
            .as_deref()
            .map_or_else(|| tilde(&base, home), str::to_string)
    );
    if suggestion.folders.len() > 1 {
        let _ = write!(out, "  ({} folders)", suggestion.folders.len());
    }
    let _ = write!(
        out,
        "\n  {} in {}",
        human_bytes(suggestion.size()),
        plural(suggestion.files.len(), "file", "files")
    );
    if suggestion.reclaimable() > 0 {
        let _ = write!(
            out,
            ", about {} could be freed",
            human_bytes(suggestion.reclaimable())
        );
    }
    let _ = writeln!(out);
    for file in suggestion.files.iter().take(LISTED_FILES) {
        render_file(out, file, &base);
    }
    if suggestion.files.len() > LISTED_FILES {
        let _ = writeln!(
            out,
            "    …and {} more",
            suggestion.files.len() - LISTED_FILES
        );
    }
    match &suggestion.pattern {
        Some(pattern) => {
            let _ = writeln!(
                out,
                "  To add it, put this line in [scan] roots in ~/.modeld/config.toml:\n    {},",
                toml_edit::Value::from(pattern.as_str())
            );
        }
        None => {
            let _ = writeln!(
                out,
                "  To add it: modeld watch {}",
                shell_word(&tilde(&base, home))
            );
        }
    }
}

fn render_file(out: &mut String, file: &Candidate, base: &Path) {
    let _ = writeln!(
        out,
        "    {:>8}  {:<14}  {}",
        human_bytes(file.size),
        match (file.likely_copy, file.reclaimable) {
            (false, _) => "",
            (true, 0) => "already shared",
            (true, _) => "likely copy",
        },
        file.path.strip_prefix(base).unwrap_or(&file.path).display()
    );
}

fn common_folder(folders: &[PathBuf]) -> PathBuf {
    let mut common = folders.first().cloned().unwrap_or_default();
    for folder in folders {
        while !folder.starts_with(&common) && common.pop() {}
    }
    common
}

/// `path` with the home folder written as `~`.
fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(relative) if relative.as_os_str().is_empty() => "~".to_string(),
        Ok(relative) => format!("~/{}", relative.display()),
        Err(_) => path.display().to_string(),
    }
}

/// `text` quoted for a shell when needed. `watch` expands a quoted `~/` itself.
fn shell_word(text: &str) -> String {
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-~+,:@".contains(c))
    {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    let digits = count.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{grouped} {}", if count == 1 { one } else { many })
}

fn duration(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/me";

    /// A file; `reclaimable` is `Some` for a likely copy.
    fn file(path: &str, size: u64, reclaimable: Option<u64>) -> Candidate {
        Candidate {
            path: PathBuf::from(path),
            size,
            likely_copy: reclaimable.is_some(),
            reclaimable: reclaimable.unwrap_or(0),
        }
    }

    #[test]
    fn renders_patterns_folders_loose_files_and_totals() {
        const NPC: u64 = 3_400_000_000;
        let discovery = Discovery {
            suggestions: vec![
                Suggestion {
                    pattern: Some("~/games/*/Assets".to_string()),
                    folders: vec![
                        PathBuf::from("/Users/me/games/Old Tavern/Assets"),
                        PathBuf::from("/Users/me/games/orc/Assets/Streaming"),
                    ],
                    files: vec![
                        file("/Users/me/games/Old Tavern/Assets/npc.gguf", NPC, None),
                        file(
                            "/Users/me/games/orc/Assets/Streaming/npc.gguf",
                            NPC,
                            Some(NPC),
                        ),
                        file(
                            "/Users/me/games/orc/Assets/Streaming/clone.gguf",
                            NPC,
                            Some(0),
                        ),
                    ],
                },
                Suggestion {
                    pattern: None,
                    folders: vec![PathBuf::from("/Users/me/my models")],
                    files: (1..=6)
                        .map(|n| {
                            let path = format!("/Users/me/my models/q{n}.gguf");
                            file(&path, 500_000_000, Some(500_000_000))
                        })
                        .collect(),
                },
            ],
            loose: vec![file("/Users/me/stray.gguf", 120_000_000, None)],
            folders_read: 1234,
            unreadable: 2,
            cloud_only: 1,
        };

        let text = render(
            &discovery,
            Path::new(HOME),
            100_000_000,
            Duration::from_secs(75),
        );

        assert_eq!(
            text,
            "2 folders not watched by ModelFS hold 13.2 GB of models; adding them could free about 6.4 GB.

~/games/*/Assets  (2 folders)
  10.2 GB in 3 files, about 3.4 GB could be freed
      3.4 GB                  Old Tavern/Assets/npc.gguf
      3.4 GB  likely copy     orc/Assets/Streaming/npc.gguf
      3.4 GB  already shared  orc/Assets/Streaming/clone.gguf
  To add it, put this line in [scan] roots in ~/.modeld/config.toml:
    \"~/games/*/Assets\",

~/my models
  3.0 GB in 6 files, about 3.0 GB could be freed
    500.0 MB  likely copy     q1.gguf
    500.0 MB  likely copy     q2.gguf
    500.0 MB  likely copy     q3.gguf
    500.0 MB  likely copy     q4.gguf
    500.0 MB  likely copy     q5.gguf
    …and 1 more
  To add it: modeld watch '~/my models'

Not suggested: these files sit directly in your home folder or a project folder, too broad to watch. Move them into a folder of their own to add them.
  120.0 MB  ~/stray.gguf

Looked through 1,234 folders in 1m 15s; 2 items could not be read; left 1 item stored only in the cloud alone.
A likely copy matches the size and sampled bytes of a stored model or another file listed; one already shared is a clone and frees nothing. Passes compare full contents before they change anything.
"
        );
    }

    #[test]
    fn says_so_when_nothing_is_unwatched() {
        let text = render(
            &Discovery {
                folders_read: 1,
                ..Discovery::default()
            },
            Path::new(HOME),
            100_000_000,
            Duration::from_secs(3),
        );

        assert_eq!(
            text,
            "No unwatched folders hold model files of 100.0 MB or more.\n\nLooked through 1 folder in 3s.\n"
        );
    }

    #[test]
    fn shell_words_are_quoted_only_when_needed() {
        assert_eq!(shell_word("~/models/bonsai2"), "~/models/bonsai2");
        assert_eq!(shell_word("~/Old Tavern"), "'~/Old Tavern'");
        assert_eq!(shell_word("~/it's"), r"'~/it'\''s'");
    }
}
