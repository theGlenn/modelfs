//! Decides which filesystem events should wake the daemon.
//!
//! The watcher only reports paths under scan roots and the store directory,
//! so the filter's job is dropping noise: modeld's own swap temp files,
//! registry and journal writes in the store (everything but `config.toml`),
//! and excluded subtrees such as the Hugging Face Xet chunk cache. `FSEvents`
//! reports symlink-resolved paths (`/private/var/...` for `/var/...`), so every
//! prefix is matched in both its configured and canonical spelling.

use modeld_core::consolidate;
use modeld_providers::ProviderRoot;
use std::path::{Path, PathBuf};

/// Relevance test for watcher paths.
#[derive(Debug)]
pub struct EventFilter {
    excluded: Vec<PathBuf>,
    store: Vec<PathBuf>,
    config: Vec<PathBuf>,
}

impl EventFilter {
    /// Builds the filter for the given scan roots and store directory.
    pub fn new(roots: &[ProviderRoot], store_root: &Path) -> Self {
        Self {
            excluded: roots
                .iter()
                .flat_map(|root| root.excluded.iter())
                .flat_map(|path| spellings(path))
                .collect(),
            store: spellings(store_root),
            config: spellings(&store_root.join("config.toml")),
        }
    }

    /// Whether a watcher event may need a pass.
    ///
    /// Reads never do; a rescan request (the kernel dropped events) or an
    /// event without paths always does, since something was missed.
    pub fn wakes_daemon(&self, event: &notify::Event) -> bool {
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return false;
        }
        event.need_rescan()
            || event.paths.is_empty()
            || event.paths.iter().any(|path| self.is_relevant(path))
    }

    /// Whether a change at `path` may need a pass.
    pub fn is_relevant(&self, path: &Path) -> bool {
        if self.config.iter().any(|config| path == config) {
            return true;
        }
        let is_swap_temp = path.file_name().is_some_and(consolidate::is_swap_temp);
        let in_store = self.store.iter().any(|store| path.starts_with(store));
        let excluded = self.excluded.iter().any(|ex| path.starts_with(ex));
        !is_swap_temp && !in_store && !excluded
    }
}

/// The path as configured plus its symlink-resolved form, when that differs.
///
/// A missing leaf (say, no `config.toml` yet) resolves through its parent.
fn spellings(path: &Path) -> Vec<PathBuf> {
    let canonical = path.canonicalize().ok().or_else(|| {
        let parent = path.parent()?.canonicalize().ok()?;
        Some(parent.join(path.file_name()?))
    });
    let mut spellings = vec![path.to_path_buf()];
    spellings.extend(canonical.filter(|canonical| canonical != path));
    spellings
}

#[cfg(test)]
mod tests {
    use super::*;
    use modeld_core::ProviderKind;

    struct Fixture {
        dir: tempfile::TempDir,
        filter: EventFilter,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("create temp dir");
        let hub = dir.path().join("hf");
        std::fs::create_dir_all(hub.join("xet")).expect("create xet dir");
        std::fs::create_dir_all(dir.path().join(".modeld")).expect("create store");
        let root = ProviderRoot {
            kind: ProviderKind::HuggingFace,
            root: hub.clone(),
            excluded: vec![hub.join("xet")],
            label_prefix: None,
        };
        let filter = EventFilter::new(&[root], &dir.path().join(".modeld"));
        Fixture { dir, filter }
    }

    fn event(kind: notify::EventKind, path: PathBuf) -> notify::Event {
        notify::Event::new(kind).add_path(path)
    }

    #[test]
    fn reads_never_wake_the_daemon_but_writes_and_rescans_do() {
        use notify::event::{AccessKind, Flag, ModifyKind};
        let f = fixture();
        let model = f.dir.path().join("hf/blobs/abc123");
        let temp = f.dir.path().join("hf/blobs/.abc123.modeld-tmp-7");

        let read = event(notify::EventKind::Access(AccessKind::Any), model.clone());
        let write = event(notify::EventKind::Modify(ModifyKind::Any), model);
        let own_swap = event(notify::EventKind::Modify(ModifyKind::Any), temp.clone());
        let rescan = event(notify::EventKind::Other, temp).set_flag(Flag::Rescan);

        assert!(!f.filter.wakes_daemon(&read));
        assert!(f.filter.wakes_daemon(&write));
        assert!(!f.filter.wakes_daemon(&own_swap));
        assert!(f.filter.wakes_daemon(&rescan));
    }

    #[test]
    fn model_file_changes_are_relevant() {
        let f = fixture();
        assert!(f.filter.is_relevant(&f.dir.path().join("hf/blobs/abc123")));
    }

    #[test]
    fn swap_temp_files_are_noise() {
        let f = fixture();
        let temp = f.dir.path().join("hf/blobs/.abc123.modeld-tmp-4242");
        assert!(!f.filter.is_relevant(&temp));
    }

    #[test]
    fn model_named_like_a_swap_temp_is_relevant() {
        let f = fixture();
        let model = f.dir.path().join("hf/blobs/model.modeld-tmp-v2.gguf");
        assert!(f.filter.is_relevant(&model));
    }

    #[test]
    fn excluded_subtrees_are_noise_in_either_spelling() {
        let f = fixture();
        let chunk = f.dir.path().join("hf/xet/chunk-cache/0001");
        let canonical = f
            .dir
            .path()
            .canonicalize()
            .expect("canonical temp dir")
            .join("hf/xet/chunk-cache/0001");

        assert!(!f.filter.is_relevant(&chunk));
        assert!(!f.filter.is_relevant(&canonical));
    }

    #[test]
    fn store_writes_are_noise_but_config_edits_are_not() {
        let f = fixture();
        let store = f.dir.path().join(".modeld");

        assert!(!f.filter.is_relevant(&store.join("registry.db-wal")));
        assert!(!f.filter.is_relevant(&store.join("blobs/sha256-abc")));
        assert!(f.filter.is_relevant(&store.join("config.toml")));
    }

    #[test]
    fn config_is_recognized_canonically_before_it_exists() {
        let f = fixture();
        let canonical_store = f
            .dir
            .path()
            .canonicalize()
            .expect("canonical temp dir")
            .join(".modeld");

        assert!(f.filter.is_relevant(&canonical_store.join("config.toml")));
    }
}
