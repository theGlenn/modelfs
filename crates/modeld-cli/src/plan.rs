//! Builds consolidation replacements anchored on canonical store blobs.

use modeld_core::consolidate::Replacement;
use modeld_core::dedup::DuplicateGroup;
use modeld_core::{Artifact, FileId};
use std::path::Path;

/// Replacements making group members clones of `canonical` (the store blob).
///
/// Policy: members hardlinked to the canonical are excluded (already share
/// storage); members on another volume are excluded (clonefile cannot cross
/// volumes). When `skip_first` is set, the first member is excluded — it is the
/// file the blob was just cloned from, so it already shares extents.
pub fn replacements_for_group(
    artifacts: &[Artifact],
    group: &DuplicateGroup,
    canonical: &Path,
    canonical_id: Option<FileId>,
    skip_first: bool,
) -> Vec<Replacement> {
    let members = if skip_first {
        &group.members[1..]
    } else {
        &group.members[..]
    };
    members
        .iter()
        .map(|&index| &artifacts[index])
        .filter(|victim| {
            let hardlinked = canonical_id.is_some() && victim.file_id == canonical_id;
            let same_device = match (canonical_id, victim.file_id) {
                (Some(a), Some(b)) => a.device == b.device,
                _ => false,
            };
            !hardlinked && same_device
        })
        .map(|victim| Replacement {
            canonical: canonical.to_path_buf(),
            victim: victim.path.clone(),
            digest: group.digest.clone(),
            size: group.size,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use modeld_core::dedup::duplicate_groups;
    use modeld_core::{Algorithm, Digest, ProviderKind};
    use std::path::PathBuf;

    fn artifact(path: &str, device: u64, inode: u64) -> Artifact {
        Artifact {
            path: PathBuf::from(path),
            size: 100,
            provider: ProviderKind::Manual,
            format: None,
            digest: Some(Digest::new(Algorithm::Sha256, vec![1; 32]).expect("valid len")),
            digest_verified: true,
            label: None,
            file_id: Some(FileId { device, inode }),
        }
    }

    #[test]
    fn all_members_become_victims_of_the_store_blob() {
        let artifacts = [artifact("/a", 1, 10), artifact("/b", 1, 11)];
        let groups = duplicate_groups(&artifacts);
        let blob = PathBuf::from("/home/.modeld/blobs/sha256-x");
        let blob_id = Some(FileId {
            device: 1,
            inode: 99,
        });

        let plan = replacements_for_group(&artifacts, &groups[0], &blob, blob_id, false);

        assert_eq!(plan.len(), 2);
        assert!(plan.iter().all(|r| r.canonical == blob));
    }

    #[test]
    fn skip_first_excludes_the_import_source() {
        let artifacts = [artifact("/source", 1, 10), artifact("/dupe", 1, 11)];
        let groups = duplicate_groups(&artifacts);
        let blob = PathBuf::from("/blobs/sha256-x");
        let blob_id = Some(FileId {
            device: 1,
            inode: 99,
        });

        let plan = replacements_for_group(&artifacts, &groups[0], &blob, blob_id, true);

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].victim, PathBuf::from("/dupe"));
    }

    #[test]
    fn excludes_cross_volume_and_hardlinked_members() {
        let artifacts = [
            artifact("/other-volume", 2, 10),
            artifact("/hardlink", 1, 99),
        ];
        let groups = duplicate_groups(&artifacts);
        let blob = PathBuf::from("/blobs/sha256-x");
        let blob_id = Some(FileId {
            device: 1,
            inode: 99,
        });

        let plan = replacements_for_group(&artifacts, &groups[0], &blob, blob_id, false);

        assert!(plan.is_empty());
    }
}
