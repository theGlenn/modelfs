//! Finds folders holding large model files that no scan root covers.
//!
//! Discovery only suggests: it never changes configuration or files. It walks
//! the home folder once, skipping folders already scanned, app data, version
//! control, dependencies, build output, and bundles, and keeps the large files
//! a scan would recognize as models. Files are grouped into the folders a
//! person would add (see [`place`]), and folders at the same place in sibling
//! projects become one glob pattern (see [`generalize`]).
//!
//! Copies are estimated without hashing whole files, which takes minutes per
//! gigabyte in the background: a file with the same size and sampled bytes as
//! a stored model, or as another file found, is a *likely copy* (see
//! [`mark_copies`]). A pass hashes before it changes anything anyway.

use crate::{Detection, scan};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

/// Smallest file discovery reports: below this, a folder is rarely worth adding.
pub const DEFAULT_MIN_SIZE: u64 = 100_000_000;

/// Folders whose parent is a project root: a repository, worktree, or Unity project.
const PROJECT_MARKERS: [&str; 2] = [".git", "ProjectSettings"];

/// Folder names never entered: version control, dependencies, and build output.
const SKIPPED_NAMES: [&str; 6] = [
    ".git",
    "node_modules",
    "target",
    "DerivedData",
    ".build",
    "Pods",
];

/// Bundles never entered: copies inside apps and archives are build output.
const SKIPPED_BUNDLES: [&str; 5] = ["app", "appex", "framework", "xcarchive", "photoslibrary"];

/// Home folders never entered. `Library` holds app data behind privacy prompts
/// and cloud storage; `.modeld` is the store.
const SKIPPED_IN_HOME: [&str; 3] = ["Library", ".Trash", ".modeld"];

/// A large model file outside every scan root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub size: u64,
    /// Same size and sampled bytes as a stored model or another candidate.
    pub likely_copy: bool,
    /// What deduplicating it would free: the bytes no other file shares.
    /// Zero unless it is a likely copy, and for a copy that is already a clone.
    pub reclaimable: u64,
}

/// One entry to add to `[scan] roots`, with the files it would bring in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// A glob pattern (`~/games/*/Assets`) when the same folder in sibling
    /// projects holds models; `None` to add the single folder in `folders`.
    pub pattern: Option<String>,
    /// The folders holding `files`, sorted.
    pub folders: Vec<PathBuf>,
    /// Largest first.
    pub files: Vec<Candidate>,
}

impl Suggestion {
    #[must_use]
    pub fn size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    /// Bytes adding it would probably free.
    #[must_use]
    pub fn reclaimable(&self) -> u64 {
        self.files.iter().map(|file| file.reclaimable).sum()
    }
}

/// What a discovery walk found.
#[derive(Debug, Default)]
pub struct Discovery {
    /// Most likely copies first, then largest.
    pub suggestions: Vec<Suggestion>,
    /// Files directly in the home folder or a project root: watching their
    /// folder would scan unrelated build output and caches.
    pub loose: Vec<Candidate>,
    pub folders_read: usize,
    /// Entries that could not be listed or inspected.
    pub unreadable: usize,
    /// Large files and folders stored only in the cloud: reading them would download them.
    pub cloud_only: usize,
}

/// Walks `home` for model files of at least `min_size` bytes that no root in
/// `detection` covers. `stored_models` are the store's blob files.
///
/// `progress` hears the number of folders read so far after each folder;
/// returning [`ControlFlow::Break`] stops the walk, and `discover` returns
/// `None`.
pub fn discover(
    home: &Path,
    detection: &Detection,
    stored_models: &[PathBuf],
    min_size: u64,
    progress: impl FnMut(usize) -> ControlFlow<()>,
) -> Option<Discovery> {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let skipped: HashSet<PathBuf> = detection
        .roots
        .iter()
        .flat_map(|root| std::iter::once(&root.root).chain(&root.excluded))
        .cloned()
        .chain(crate::protected_paths(&home))
        .map(|path| crate::canonical_prefix(&path))
        .chain(SKIPPED_IN_HOME.iter().map(|name| home.join(name)))
        .collect();
    let mut walk = walk(&home, &skipped, min_size, progress)?;
    let candidates = mark_copies(std::mem::take(&mut walk.files), stored_models);
    let mut placed = Vec::new();
    let mut loose = Vec::new();
    if !candidates.is_empty() {
        place(&home, &walk.projects, candidates, &mut placed, &mut loose);
    }
    let mut suggestions = generalize(&home, &walk.projects, placed);
    suggestions.sort_by(|a, b| {
        b.reclaimable()
            .cmp(&a.reclaimable())
            .then(b.size().cmp(&a.size()))
            .then(a.folders.cmp(&b.folders))
    });
    loose.sort_by(|a, b| a.path.cmp(&b.path));
    Some(Discovery {
        suggestions,
        loose,
        folders_read: walk.folders_read,
        unreadable: walk.unreadable,
        cloud_only: walk.cloud_only,
    })
}

#[derive(Debug, Default)]
struct Walk {
    files: Vec<(PathBuf, u64)>,
    projects: HashSet<PathBuf>,
    folders_read: usize,
    unreadable: usize,
    cloud_only: usize,
}

fn walk(
    home: &Path,
    skipped: &HashSet<PathBuf>,
    min_size: u64,
    mut progress: impl FnMut(usize) -> ControlFlow<()>,
) -> Option<Walk> {
    let mut projects = HashSet::new();
    let mut cloud_only_folders = 0;
    let mut found = Walk::default();
    let entries = walkdir::WalkDir::new(home)
        .follow_links(false)
        // Clones never cross volumes, so models on other disks cannot share bytes.
        .same_file_system(true)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name();
            if PROJECT_MARKERS.iter().any(|marker| name == *marker)
                && let Some(project) = entry.path().parent()
            {
                projects.insert(project.to_path_buf());
            }
            if !entry.file_type().is_dir() || entry.depth() == 0 {
                return true;
            }
            let path = entry.path();
            let bundle = path
                .extension()
                .is_some_and(|extension| SKIPPED_BUNDLES.iter().any(|b| extension == *b));
            if bundle || SKIPPED_NAMES.iter().any(|skip| name == *skip) || skipped.contains(path) {
                return false;
            }
            // Listing a folder that lives only in the cloud downloads it.
            let cloud_only = entry
                .metadata()
                .is_ok_and(|metadata| is_dataless(&metadata));
            cloud_only_folders += usize::from(cloud_only);
            !cloud_only
        });
    for entry in entries {
        let Ok(entry) = entry else {
            found.unreadable += 1;
            continue;
        };
        if entry.file_type().is_dir() {
            found.folders_read += 1;
            if progress(found.folders_read).is_break() {
                return None;
            }
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            found.unreadable += 1;
            continue;
        };
        if metadata.len() < min_size {
            continue;
        }
        if is_dataless(&metadata) {
            found.cloud_only += 1;
        } else if looks_like_model(entry.path()) {
            found.files.push((entry.into_path(), metadata.len()));
        }
    }
    found.projects = projects;
    found.cloud_only += cloud_only_folders;
    Some(found)
}

/// Whether a scan would record `path` as a model.
///
/// `.bin` and `.pt` must also start like weights (GGUF, GGML, or the zip or
/// pickle torch writes): other tools use these extensions for caches and databases,
/// such as Unity's `Library/DataStore`, and a folder of those is not a model
/// folder.
fn looks_like_model(path: &Path) -> bool {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("bin" | "pt") => {
            let mut magic = [0u8; 4];
            std::fs::File::open(path)
                .and_then(|mut file| file.read_exact(&mut magic))
                .is_ok()
                && (matches!(
                    &magic,
                    b"GGUF" | b"lmgg" | b"fmgg" | b"tjgg" | b"PK\x03\x04"
                ) || (magic[0] == 0x80 && (2..=5).contains(&magic[1])))
        }
        _ => scan::detect_format(path).is_some(),
    }
}

#[cfg(target_os = "macos")]
fn is_dataless(metadata: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    /// `SF_DATALESS` in `<sys/stat.h>`: the contents are not on this Mac.
    const SF_DATALESS: u32 = 0x4000_0000;
    metadata.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn is_dataless(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Flags each file that looks like a copy of a stored model or of an earlier
/// file, and what deduplicating it would free.
///
/// Equal sizes alone are not enough: fine-tunes sharing an architecture and
/// quantization have the same byte size but different weights. So files of
/// the same size also compare evenly spaced samples of their bytes. A copy
/// that is already an APFS clone shares its blocks and frees nothing.
fn mark_copies(mut files: Vec<(PathBuf, u64)>, stored_models: &[PathBuf]) -> Vec<Candidate> {
    files.sort();
    let mut stored: HashMap<u64, Vec<&Path>> = HashMap::new();
    for model in stored_models {
        if let Ok(metadata) = std::fs::metadata(model) {
            stored.entry(metadata.len()).or_default().push(model);
        }
    }
    let mut same_size: HashMap<u64, usize> = HashMap::new();
    for (_, size) in &files {
        *same_size.entry(*size).or_default() += 1;
    }
    // Samples of the stored models and earlier files, by size.
    let mut known: HashMap<u64, Vec<Vec<u8>>> = HashMap::new();
    files
        .into_iter()
        .map(|(path, size)| {
            let has_peers = stored.contains_key(&size) || same_size[&size] > 1;
            let likely_copy = has_peers
                && sample(&path, size).is_some_and(|own| {
                    let known = known.entry(size).or_insert_with(|| {
                        stored
                            .get(&size)
                            .into_iter()
                            .flatten()
                            .filter_map(|model| sample(model, size))
                            .collect()
                    });
                    let copy = known.contains(&own);
                    if !copy {
                        known.push(own);
                    }
                    copy
                });
            Candidate {
                reclaimable: if likely_copy {
                    unshared_bytes(&path, size)
                } else {
                    0
                },
                likely_copy,
                path,
                size,
            }
        })
        .collect()
}

/// Sixteen 4 KiB samples spread evenly over a file of `size` bytes.
fn sample(path: &Path, size: u64) -> Option<Vec<u8>> {
    const SAMPLES: u64 = 16;
    const SAMPLE_BYTES: u64 = 4096;
    let mut file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    for index in 0..SAMPLES {
        let offset = size.saturating_sub(SAMPLE_BYTES) * index / (SAMPLES - 1);
        file.seek(SeekFrom::Start(offset)).ok()?;
        (&mut file)
            .take(SAMPLE_BYTES)
            .read_to_end(&mut bytes)
            .ok()?;
    }
    Some(bytes)
}

/// The bytes of `path` no other file shares, at most `size`.
#[cfg(target_os = "macos")]
fn unshared_bytes(path: &Path, size: u64) -> u64 {
    modeld_core::apfs::private_size(path).map_or(size, |private| private.min(size))
}

#[cfg(not(target_os = "macos"))]
fn unshared_bytes(_path: &Path, size: u64) -> u64 {
    size
}

/// Groups `files` into the folders a person would add.
///
/// A group's folder is the deepest one holding all its files, unless that
/// folder is too broad to watch: the home folder, a project root, or a folder
/// containing a project, where a scan would also read build output and caches.
/// Then files directly in it are `loose`, and the rest split by the subfolder
/// they are in, each part placed again.
fn place(
    home: &Path,
    projects: &HashSet<PathBuf>,
    files: Vec<Candidate>,
    placed: &mut Vec<(PathBuf, Vec<Candidate>)>,
    loose: &mut Vec<Candidate>,
) {
    let root = deepest_common_folder(files.iter().filter_map(|file| file.path.parent()));
    let too_broad = root == home || projects.iter().any(|project| project.starts_with(&root));
    if !too_broad {
        placed.push((root, files));
        return;
    }
    let mut parts: BTreeMap<OsString, Vec<Candidate>> = BTreeMap::new();
    for file in files {
        let next = file
            .path
            .strip_prefix(&root)
            .ok()
            .and_then(|relative| relative.parent())
            .and_then(|folder| folder.iter().next())
            .map(ToOwned::to_owned);
        match next {
            Some(next) => parts.entry(next).or_default().push(file),
            None => loose.push(file),
        }
    }
    for part in parts.into_values() {
        place(home, projects, part, placed, loose);
    }
}

fn deepest_common_folder<'a>(mut folders: impl Iterator<Item = &'a Path>) -> PathBuf {
    let Some(first) = folders.next() else {
        return PathBuf::new();
    };
    let mut common = first.to_path_buf();
    for folder in folders {
        while !folder.starts_with(&common) && common.pop() {}
    }
    common
}

/// Turns placed folders into suggestions, merging folders at the same place
/// in sibling projects into one glob pattern so projects created later are
/// covered too: `~/games/alpha/Assets` and `~/games/beta/Assets/Streaming`
/// become `~/games/*/Assets`.
///
/// Projects directly in the home folder are not merged: `~/*/Assets` would
/// match far more than those projects.
fn generalize(
    home: &Path,
    projects: &HashSet<PathBuf>,
    placed: Vec<(PathBuf, Vec<Candidate>)>,
) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();
    let mut siblings: BTreeMap<(PathBuf, OsString), Vec<Member>> = BTreeMap::new();
    for (folder, files) in placed {
        // Keyed by the folder holding the project and the project subfolder.
        let sibling = folder
            .ancestors()
            .take_while(|ancestor| *ancestor != home)
            .find(|ancestor| projects.contains(*ancestor))
            .and_then(|project| {
                let parent = project.parent().filter(|parent| *parent != home)?;
                let first = folder.strip_prefix(project).ok()?.iter().next()?;
                Some((
                    (parent.to_path_buf(), first.to_owned()),
                    project.to_path_buf(),
                ))
            });
        match sibling {
            Some((key, project)) => siblings.entry(key).or_default().push(Member {
                project,
                folder,
                files,
            }),
            None => suggestions.push(suggestion(None, vec![folder], files)),
        }
    }
    for ((parent, _), members) in siblings {
        let distinct: HashSet<_> = members.iter().map(|member| &member.project).collect();
        if distinct.len() < 2 {
            for member in members {
                suggestions.push(suggestion(None, vec![member.folder], member.files));
            }
            continue;
        }
        let shared = deepest_common_folder(
            members
                .iter()
                .filter_map(|member| member.folder.strip_prefix(&member.project).ok()),
        );
        let pattern = Path::new("~")
            .join(escaped(parent.strip_prefix(home).unwrap_or(&parent)))
            .join("*")
            .join(escaped(&shared));
        let mut folders = Vec::new();
        let mut files = Vec::new();
        for member in members {
            folders.push(member.folder);
            files.extend(member.files);
        }
        suggestions.push(suggestion(
            Some(pattern.to_string_lossy().into_owned()),
            folders,
            files,
        ));
    }
    suggestions
}

/// A placed folder inside a project, before merging with its siblings.
struct Member {
    project: PathBuf,
    folder: PathBuf,
    files: Vec<Candidate>,
}

/// `path` with glob metacharacters in each name escaped.
fn escaped(path: &Path) -> PathBuf {
    path.iter()
        .map(|name| glob::Pattern::escape(&name.to_string_lossy()))
        .collect()
}

fn suggestion(
    pattern: Option<String>,
    mut folders: Vec<PathBuf>,
    mut files: Vec<Candidate>,
) -> Suggestion {
    folders.sort();
    files.sort_by(|a, b| b.size.cmp(&a.size).then(a.path.cmp(&b.path)));
    Suggestion {
        pattern,
        folders,
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderRoot;
    use crate::config::{Config, ScanConfig};
    use modeld_core::ProviderKind;

    /// A home folder whose model files are a few bytes: tests use `min_size` 1.
    struct Home {
        dir: tempfile::TempDir,
        path: PathBuf,
    }

    impl Home {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("home");
            let path = dir.path().canonicalize().expect("canonical home");
            Self { dir, path }
        }

        fn file(&self, relative: &str, content: &[u8]) -> PathBuf {
            let path = self.path.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, content).expect("write");
            path
        }

        fn model(&self, relative: &str) -> PathBuf {
            self.file(relative, b"GGUF weights")
        }

        fn project(&self, relative: &str) {
            std::fs::create_dir_all(self.path.join(relative).join(".git")).expect("git");
        }

        fn discover(&self, detection: &Detection) -> Discovery {
            discover(self.dir.path(), detection, &[], 1, |_| {
                ControlFlow::Continue(())
            })
            .expect("not stopped")
        }
    }

    fn folders(discovery: &Discovery) -> Vec<(Option<&str>, Vec<&Path>)> {
        discovery
            .suggestions
            .iter()
            .map(|suggestion| {
                (
                    suggestion.pattern.as_deref(),
                    suggestion.folders.iter().map(PathBuf::as_path).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn skips_watched_folders_app_data_build_output_and_non_models() {
        let home = Home::new();
        let wanted = home.model("models/a.gguf");
        home.model("watched/b.gguf");
        home.model("Library/Application Support/App/c.gguf");
        home.model(".Trash/d.gguf");
        home.model(".modeld/blobs/sha256-e");
        home.model(".lmstudio/.internal/staging/f.gguf");
        home.model("code/target/debug/g.gguf");
        home.model("code/node_modules/pkg/h.gguf");
        home.model("Build/Game.app/Contents/Resources/i.gguf");
        home.file("models/notes.txt", b"not a model at all");
        home.file("models/cache.bin", b"\0\0\0\0 database pages");
        let whisper = home.file("models/ggml-small.bin", b"lmgg whisper");
        let detection = Detection {
            roots: vec![ProviderRoot {
                kind: ProviderKind::Manual,
                root: home.path.join("watched"),
                excluded: Vec::new(),
                label_prefix: None,
            }],
            unreadable: Vec::new(),
        };

        let discovery = home.discover(&detection);

        assert_eq!(
            folders(&discovery),
            [(None, vec![home.path.join("models").as_path()])]
        );
        let files: Vec<_> = discovery.suggestions[0]
            .files
            .iter()
            .map(|file| &file.path)
            .collect();
        assert_eq!(files, [&wanted, &whisper]);
        assert!(discovery.loose.is_empty());
    }

    #[test]
    fn stops_below_project_roots_and_merges_sibling_projects_into_a_pattern() {
        let home = Home::new();
        home.project("games/alpha");
        home.project("games/beta");
        home.project("games/gamma");
        home.model("games/alpha/Assets/Models/a.gguf");
        home.model("games/alpha/Assets/Streaming/Voices/b.gguf");
        home.model("games/alpha/Library/Cache/c.gguf");
        home.model("games/beta/Assets/Streaming/d.gguf");
        home.model("games/notes/e.gguf");

        let discovery = home.discover(&Detection::default());

        let games = home.path.join("games");
        assert_eq!(
            folders(&discovery),
            [
                (
                    Some("~/games/*/Assets"),
                    vec![
                        games.join("alpha/Assets").as_path(),
                        games.join("beta/Assets/Streaming").as_path(),
                    ]
                ),
                (None, vec![games.join("alpha/Library/Cache").as_path()]),
                (None, vec![games.join("notes").as_path()]),
            ]
        );
    }

    #[test]
    fn a_suggested_pattern_matches_its_folders_even_with_glob_characters() {
        let home = Home::new();
        home.project("my [games]/alpha");
        home.project("my [games]/beta*");
        home.model("my [games]/alpha/Assets/a.gguf");
        home.model("my [games]/beta*/Assets/b.gguf");

        let discovery = home.discover(&Detection::default());

        let suggestion = &discovery.suggestions[0];
        let pattern = suggestion.pattern.clone().expect("pattern");
        let config = Config {
            scan: ScanConfig {
                roots: vec![pattern],
            },
        };
        let roots: Vec<_> = crate::config::manual_roots(&config, home.dir.path())
            .into_iter()
            .map(|root| root.root)
            .collect();
        assert_eq!(roots, suggestion.folders);
    }

    #[test]
    fn files_in_home_or_a_project_root_are_loose() {
        let home = Home::new();
        home.project("repo");
        let in_home = home.model("model.gguf");
        let in_repo = home.model("repo/weights.gguf");
        home.model("repo/fixtures/models/x.gguf");

        let discovery = home.discover(&Detection::default());

        let loose: Vec<_> = discovery.loose.iter().map(|file| &file.path).collect();
        assert_eq!(loose, [&in_home, &in_repo]);
        assert_eq!(
            folders(&discovery),
            [(None, vec![home.path.join("repo/fixtures/models").as_path()])]
        );
    }

    #[test]
    fn folders_without_projects_merge_into_their_common_folder() {
        let home = Home::new();
        home.model("models/bonsai/a.gguf");
        home.model("models/qwen/deep/b.gguf");

        let discovery = home.discover(&Detection::default());

        assert_eq!(
            folders(&discovery),
            [(None, vec![home.path.join("models").as_path()])]
        );
    }

    /// One MiB of `byte`, written to disk so APFS reports its blocks.
    fn weights(path: &Path, byte: u8) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let mut file = std::fs::File::create(path).expect("create");
        std::io::Write::write_all(&mut file, &vec![byte; 1 << 20]).expect("write");
        file.sync_all().expect("sync");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn copies_need_matching_samples_and_clones_free_nothing() {
        let home = Home::new();
        let store = tempfile::tempdir().expect("store");
        let stored = store.path().join("sha256-stored");
        weights(&stored, 1);
        weights(&home.path.join("stored/copy.gguf"), 1);
        // Same size, different weights: fine-tunes of one architecture.
        weights(&home.path.join("finetunes/base.gguf"), 2);
        weights(&home.path.join("finetunes/tuned.gguf"), 3);
        let original = home.path.join("pair/a.gguf");
        weights(&original, 4);
        weights(&home.path.join("pair/b.gguf"), 4);
        modeld_core::apfs::clone_file(&original, &home.path.join("pair/c.gguf")).expect("clone");

        let discovery = discover(home.dir.path(), &Detection::default(), &[stored], 1, |_| {
            ControlFlow::Continue(())
        })
        .expect("not stopped");

        let summary: Vec<_> = discovery
            .suggestions
            .iter()
            .map(|suggestion| {
                suggestion
                    .files
                    .iter()
                    .map(|file| {
                        (
                            file.path
                                .strip_prefix(&home.path)
                                .expect("in home")
                                .to_owned(),
                            file.likely_copy,
                            file.reclaimable > 0,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(
            summary,
            [
                vec![
                    (PathBuf::from("pair/a.gguf"), false, false),
                    (PathBuf::from("pair/b.gguf"), true, true),
                    (PathBuf::from("pair/c.gguf"), true, false),
                ],
                vec![(PathBuf::from("stored/copy.gguf"), true, true)],
                vec![
                    (PathBuf::from("finetunes/base.gguf"), false, false),
                    (PathBuf::from("finetunes/tuned.gguf"), false, false),
                ],
            ]
        );
    }

    #[test]
    fn files_below_the_minimum_size_are_ignored() {
        let home = Home::new();
        home.model("models/small.gguf");

        let mut heard = Vec::new();
        let discovery = discover(home.dir.path(), &Detection::default(), &[], 1000, |read| {
            heard.push(read);
            ControlFlow::Continue(())
        })
        .expect("not stopped");

        assert!(discovery.suggestions.is_empty());
        assert_eq!(heard, [1, 2]);
        assert_eq!(discovery.folders_read, 2);
    }

    #[test]
    fn progress_can_stop_the_walk() {
        let home = Home::new();
        home.model("models/a.gguf");

        let stopped = discover(home.dir.path(), &Detection::default(), &[], 1, |_| {
            ControlFlow::Break(())
        });

        assert!(stopped.is_none());
    }
}
