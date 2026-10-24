//! About how much space a removal frees.
//!
//! Files that share blocks form a group: the stored copy with its clones
//! (recorded, or found by block sampling), and hard links of one inode.
//! A group's blocks are freed only when every member goes; a trashed member
//! keeps them until the Trash is emptied. When only some members go, each
//! removed file frees its private bytes (`ATTR_CMNEXT_PRIVATESIZE`: the
//! blocks no clone shares), and the rest stays pinned by the others. The
//! numbers are estimates, worded "about": blocks shared only among removed
//! clones, or a clone another tool made, are not seen.

use super::{Collateral, PlannedModel};
use crate::inventory::{self, ReferenceState, StoredCopy};
use modeld_core::FileId;
use modeld_store::Store;
use std::collections::{HashMap, HashSet};
use std::ops::AddAssign;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// How a removed path goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Removal {
    Delete,
    Trash,
}

/// About how much space a removal frees, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Frees {
    /// Freed when the removal runs.
    pub now: u64,
    /// Freed once the Trash is emptied.
    pub after_trash: u64,
    /// Bytes of removed files that stay used, because other files share them.
    pub pinned: u64,
}

impl AddAssign for Frees {
    fn add_assign(&mut self, other: Self) {
        self.now += other.now;
        self.after_trash += other.after_trash;
        self.pinned += other.pinned;
    }
}

impl Frees {
    fn add(&mut self, removal: Removal, bytes: u64) {
        match removal {
            Removal::Delete => self.now += bytes,
            Removal::Trash => self.after_trash += bytes,
        }
    }
}

/// A file in one model's sharing picture.
#[derive(Debug, Clone)]
struct Member {
    path: PathBuf,
    id: FileId,
    /// Shares the stored copy's blocks.
    anchored: bool,
    removal: Option<Removal>,
}

/// Which files share blocks with which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Group {
    /// The stored copy and its clones.
    Anchor,
    /// One inode and its names.
    Inode(FileId),
}

/// Estimates what removing `removals` (and `collateral`) frees.
pub(super) fn frees(
    store: &Store,
    models: &[PlannedModel],
    removals: &HashMap<PathBuf, Removal>,
    collateral: &[(Collateral, Removal)],
) -> Frees {
    let mut frees = Frees::default();
    for model in models {
        frees += model_frees(store, model, removals);
    }
    for (item, removal) in collateral {
        frees.add(*removal, item.size);
    }
    frees
}

fn model_frees(store: &Store, model: &PlannedModel, removals: &HashMap<PathBuf, Removal>) -> Frees {
    let Ok(Some(artifact)) = store.artifact(&model.digest) else {
        return Frees::default();
    };
    let blob = store.blob_path(&model.digest);
    let stored = StoredCopy {
        id: file_id(&blob),
        path: blob.clone(),
    };
    let mut members = Vec::new();
    if let Some(id) = stored.id {
        members.push(Member {
            path: blob,
            id,
            anchored: true,
            removal: model.everywhere.then_some(Removal::Delete),
        });
    }
    for reference in &artifact.references {
        let (state, id) = inventory::locate(store, &model.digest, &stored, &reference.path);
        let Some(id) = id.filter(|_| state != ReferenceState::Missing) else {
            continue;
        };
        members.push(Member {
            path: reference.path.clone(),
            id,
            anchored: state == ReferenceState::Clone || Some(id) == stored.id,
            removal: removals.get(&reference.path).copied(),
        });
    }
    let referenced: HashSet<&PathBuf> = artifact.references.iter().map(|r| &r.path).collect();
    for path in store.live_shared_paths(&model.digest).unwrap_or_default() {
        if let Some(id) = file_id(&path).filter(|_| !referenced.contains(&path)) {
            members.push(Member {
                path,
                id,
                anchored: true,
                removal: None,
            });
        }
    }
    group_frees(model.size, &members, |path| {
        modeld_core::apfs::private_size(path).unwrap_or(0)
    })
}

/// Sums what each sharing group frees; `private` measures one file.
fn group_frees(size: u64, members: &[Member], private: impl Fn(&Path) -> u64) -> Frees {
    let mut groups: HashMap<Group, Vec<&Member>> = HashMap::new();
    for member in members {
        let group = if member.anchored {
            Group::Anchor
        } else {
            Group::Inode(member.id)
        };
        groups.entry(group).or_default().push(member);
    }
    let mut frees = Frees::default();
    for group in groups.values() {
        let removed: Vec<&Member> = group
            .iter()
            .copied()
            .filter(|member| member.removal.is_some())
            .collect();
        if removed.is_empty() {
            continue;
        }
        if removed.len() == group.len() {
            frees.add(how(&removed), size);
            continue;
        }
        let mut released = 0;
        let inodes: HashSet<FileId> = removed.iter().map(|member| member.id).collect();
        for inode in inodes {
            let names: Vec<&Member> = group
                .iter()
                .copied()
                .filter(|member| member.id == inode)
                .collect();
            if names.iter().all(|member| member.removal.is_some()) {
                let bytes = private(&names[0].path).min(size);
                frees.add(how(&names), bytes);
                released += bytes;
            }
        }
        frees.pinned += size.saturating_sub(released);
    }
    frees
}

/// Trash when any of `removed` is trashed: its blocks wait for the Trash.
fn how(removed: &[&Member]) -> Removal {
    if removed
        .iter()
        .any(|member| member.removal == Some(Removal::Trash))
    {
        Removal::Trash
    } else {
        Removal::Delete
    }
}

fn file_id(path: &Path) -> Option<FileId> {
    std::fs::symlink_metadata(path).ok().map(|metadata| FileId {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: u64 = 1000;

    fn member(name: &str, inode: u64, anchored: bool, removal: Option<Removal>) -> Member {
        Member {
            path: PathBuf::from(name),
            id: FileId { device: 1, inode },
            anchored,
            removal,
        }
    }

    fn no_private(_: &Path) -> u64 {
        0
    }

    #[test]
    fn removing_one_clone_of_several_frees_nothing_and_pins_its_bytes() {
        let members = [
            member("/blob", 1, true, None),
            member("/lmstudio", 2, true, Some(Removal::Trash)),
            member("/hf", 3, true, None),
        ];

        let frees = group_frees(SIZE, &members, no_private);

        assert_eq!(
            frees,
            Frees {
                now: 0,
                after_trash: 0,
                pinned: SIZE
            }
        );
    }

    #[test]
    fn removing_every_copy_and_the_stored_one_frees_the_model() {
        let members = [
            member("/blob", 1, true, Some(Removal::Delete)),
            member("/hf", 2, true, Some(Removal::Delete)),
            member("/ollama", 3, true, Some(Removal::Delete)),
        ];

        assert_eq!(group_frees(SIZE, &members, no_private).now, SIZE);
    }

    #[test]
    fn a_trashed_member_delays_the_saving_until_the_trash_is_emptied() {
        let members = [
            member("/blob", 1, true, Some(Removal::Delete)),
            member("/folder", 2, true, Some(Removal::Trash)),
        ];

        let frees = group_frees(SIZE, &members, no_private);

        assert_eq!((frees.now, frees.after_trash), (0, SIZE));
    }

    #[test]
    fn an_independent_copy_frees_its_own_blocks() {
        let members = [
            member("/blob", 1, true, None),
            member("/copy", 5, false, Some(Removal::Trash)),
        ];

        assert_eq!(group_frees(SIZE, &members, no_private).after_trash, SIZE);
    }

    #[test]
    fn a_diverged_clone_frees_its_private_bytes() {
        let members = [
            member("/blob", 1, true, None),
            member("/diverged", 2, true, Some(Removal::Delete)),
        ];

        let frees = group_frees(SIZE, &members, |_| 300);

        assert_eq!(
            frees,
            Frees {
                now: 300,
                after_trash: 0,
                pinned: 700
            }
        );
    }

    #[test]
    fn one_name_of_a_hard_linked_file_frees_nothing() {
        let members = [
            member("/a", 7, false, Some(Removal::Trash)),
            member("/b", 7, false, None),
        ];

        let frees = group_frees(SIZE, &members, |_| SIZE);

        assert_eq!(frees.after_trash, 0);
        assert_eq!(frees.pinned, SIZE);
    }
}
