//! User configuration: extra scan roots beyond the known provider caches.
//!
//! Lives at `~/.modeld/config.toml`:
//!
//! ```toml
//! [scan]
//! # Extra directories to scan for model files. Glob patterns and ~ allowed.
//! roots = [
//!     "~/conductor/workspaces/*/*/fixtures/models",
//! ]
//! ```
//!
//! Matched directories become [`ProviderKind::Manual`] roots: scanned, synced, and
//! deduplicated like any provider cache. A per-project `.modeld` drop-in file may
//! supplement this later (see DECISIONS.md, "Not built yet").

use crate::ProviderRoot;
use modeld_core::ProviderKind;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub scan: ScanConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct ScanConfig {
    /// Directories (or glob patterns) to scan in addition to provider caches.
    #[serde(default)]
    pub roots: Vec<String>,
}

/// Loads the config file; a missing file is an empty config.
///
/// # Errors
/// Returns a description of an unreadable or unparseable config file — callers
/// treat its roots as unknown, never crash.
pub fn load(path: &Path) -> Result<Config, String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(error) => return Err(format!("config unreadable: {error}")),
    };
    toml::from_str(&contents).map_err(|error| format!("invalid config: {error}"))
}

/// Expands configured roots (tilde + glob) into Manual provider roots.
///
/// Only existing directories qualify; labels are prefixed with the home-relative
/// root path so `ls` shows which project a fixture file belongs to.
#[must_use]
pub fn manual_roots(config: &Config, home: &Path) -> Vec<ProviderRoot> {
    // Canonical home so label prefixes strip cleanly (macOS: /tmp vs /private/tmp).
    let home_canonical = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let mut roots = Vec::new();
    for pattern in &config.scan.roots {
        let expanded = expand_home(pattern, home);
        let Some(pattern_text) = expanded.to_str() else {
            continue;
        };
        let Ok(matches) = glob::glob(pattern_text) else {
            continue;
        };
        for path in matches.flatten().filter(|path| path.is_dir()) {
            // Canonicalize so symlink-aliased project dirs (common with Conductor
            // workspaces) collapse to one root instead of double-counting files.
            let Ok(canonical) = path.canonicalize() else {
                continue;
            };
            let label_prefix = canonical
                .strip_prefix(&home_canonical)
                .unwrap_or(&canonical)
                .to_string_lossy()
                .into_owned();
            roots.push(ProviderRoot {
                kind: ProviderKind::Manual,
                root: canonical,
                excluded: vec![],
                label_prefix: Some(label_prefix),
            });
        }
    }
    roots.sort_by(|a, b| a.root.cmp(&b.root));
    roots.dedup_by(|a, b| a.root == b.root);
    roots
}

fn expand_home(pattern: &str, home: &Path) -> PathBuf {
    pattern
        .strip_prefix("~/")
        .map_or_else(|| PathBuf::from(pattern), |relative| home.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_file_is_empty_config() {
        let config = load(Path::new("/nonexistent/config.toml")).expect("default");
        assert!(config.scan.roots.is_empty());
    }

    #[test]
    fn parses_scan_roots() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[scan]\nroots = [\"~/models\"]\n").expect("write");
        let config = load(&path).expect("parse");
        assert_eq!(config.scan.roots, vec!["~/models"]);
    }

    #[test]
    fn invalid_config_reports_instead_of_crashing() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "not [valid toml").expect("write");
        assert!(load(&path).is_err());
    }

    #[test]
    fn symlink_aliased_roots_collapse_to_one() {
        let home = tempfile::tempdir().expect("create temp dir");
        let real = home.path().join("ws/curitiba/fixtures/models");
        std::fs::create_dir_all(&real).expect("mkdir");
        std::os::unix::fs::symlink(
            home.path().join("ws/curitiba"),
            home.path().join("ws/neutrino-alias"),
        )
        .expect("symlink project dir");

        let config = Config {
            scan: ScanConfig {
                roots: vec!["~/ws/*/fixtures/models".to_string()],
            },
        };
        let roots = manual_roots(&config, home.path());

        assert_eq!(roots.len(), 1);
    }

    #[test]
    fn glob_roots_expand_to_existing_directories_with_labels() {
        let home = tempfile::tempdir().expect("create temp dir");
        let a = home.path().join("ws/roseau/fixtures/models");
        let b = home.path().join("ws/bozeman/fixtures/models");
        std::fs::create_dir_all(&a).expect("mkdir");
        std::fs::create_dir_all(&b).expect("mkdir");

        let config = Config {
            scan: ScanConfig {
                roots: vec!["~/ws/*/fixtures/models".to_string()],
            },
        };
        let roots = manual_roots(&config, home.path());

        assert_eq!(roots.len(), 2);
        assert!(roots.iter().all(|r| r.kind == ProviderKind::Manual));
        assert_eq!(
            roots[0].label_prefix.as_deref(),
            Some("ws/bozeman/fixtures/models")
        );
    }
}
