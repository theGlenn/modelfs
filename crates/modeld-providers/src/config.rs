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
//! deduplicated like any provider cache. A future `.modeld` drop-in file per project
//! will supplement this (see DECISIONS.md).

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
/// surface it as a warning, never a crash.
pub fn load(path: &Path) -> Result<Config, String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    toml::from_str(&contents).map_err(|error| format!("invalid {}: {error}", path.display()))
}

/// Expands configured roots (tilde + glob) into Manual provider roots.
///
/// Only existing directories qualify; labels are prefixed with the home-relative
/// root path so `ls` shows which project a fixture file belongs to.
#[must_use]
pub fn manual_roots(config: &Config, home: &Path) -> Vec<ProviderRoot> {
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
            let label_prefix = path
                .strip_prefix(home)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            roots.push(ProviderRoot {
                kind: ProviderKind::Manual,
                root: path,
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
