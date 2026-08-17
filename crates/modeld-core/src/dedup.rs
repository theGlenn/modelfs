//! Exact-duplicate detection over scanned artifacts.
//!
//! Two-phase by design so the expensive step is explicit and minimal:
//!
//! 1. [`indices_needing_digest`] — which artifacts must be hashed at all. Only files
//!    whose size collides with another file can possibly be duplicates, so everything
//!    else skips hashing entirely.
//! 2. [`duplicate_groups`] — once digests are present, group byte-identical files.
//!
//! Hardlinked paths (equal [`FileId`]) already share storage and count once toward
//! reclaimable space.

use crate::artifact::{Artifact, FileId};
use crate::digest::Digest;
use std::collections::{HashMap, HashSet};

/// Byte-identical artifacts stored more than once.
#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub digest: Digest,
    pub size: u64,
    /// Indices into the scanned artifact slice, in input order.
    pub members: Vec<usize>,
    /// Bytes freed if all distinct copies were consolidated into one.
    pub reclaimable: u64,
}

/// Aggregate numbers for a scan report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub artifact_count: usize,
    pub total_bytes: u64,
    pub reclaimable_bytes: u64,
}

/// Returns indices of artifacts that need hashing before duplicate detection.
///
/// An artifact needs hashing when it has no digest yet and at least one other
/// artifact has the same size (a digest can only ever match within equal sizes).
#[must_use]
pub fn indices_needing_digest(artifacts: &[Artifact]) -> Vec<usize> {
    let mut by_size: HashMap<u64, usize> = HashMap::new();
    for artifact in artifacts {
        *by_size.entry(artifact.size).or_insert(0) += 1;
    }
    artifacts
        .iter()
        .enumerate()
        .filter(|(_, a)| a.digest.is_none() && by_size[&a.size] > 1)
        .map(|(i, _)| i)
        .collect()
}

/// Groups artifacts with equal digests, ignoring artifacts without one.
///
/// Groups are returned largest-reclaimable first. Members hardlinked to the same
/// inode count as one stored copy.
#[must_use]
pub fn duplicate_groups(artifacts: &[Artifact]) -> Vec<DuplicateGroup> {
    let mut by_digest: HashMap<&Digest, Vec<usize>> = HashMap::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        if let Some(digest) = &artifact.digest {
            by_digest.entry(digest).or_default().push(index);
        }
    }

    let mut groups: Vec<DuplicateGroup> = by_digest
        .into_iter()
        .filter(|(_, members)| members.len() > 1)
        .map(|(digest, members)| {
            let size = artifacts[members[0]].size;
            let copies = distinct_storage_copies(artifacts, &members);
            DuplicateGroup {
                digest: digest.clone(),
                size,
                reclaimable: size * (copies - 1) as u64,
                members,
            }
        })
        .collect();
    groups.sort_by_key(|group| std::cmp::Reverse(group.reclaimable));
    groups
}

/// Sums a scan into headline numbers.
#[must_use]
pub fn summarize(artifacts: &[Artifact], groups: &[DuplicateGroup]) -> Summary {
    Summary {
        artifact_count: artifacts.len(),
        total_bytes: artifacts.iter().map(|a| a.size).sum(),
        reclaimable_bytes: groups.iter().map(|g| g.reclaimable).sum(),
    }
}

/// Counts group members that occupy their own storage (hardlinks count once).
///
/// Members without a known [`FileId`] are conservatively assumed distinct.
fn distinct_storage_copies(artifacts: &[Artifact], members: &[usize]) -> usize {
    let mut seen: HashSet<FileId> = HashSet::new();
    let mut copies = 0;
    for &index in members {
        match artifacts[index].file_id {
            Some(id) => {
                if seen.insert(id) {
                    copies += 1;
                }
            }
            None => copies += 1,
        }
    }
    copies
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{Format, ProviderKind};
    use crate::digest::Algorithm;
    use std::path::PathBuf;

    fn artifact(path: &str, size: u64, digest_byte: Option<u8>, inode: Option<u64>) -> Artifact {
        Artifact {
            path: PathBuf::from(path),
            size,
            provider: ProviderKind::Manual,
            format: Some(Format::Gguf),
            digest: digest_byte.map(|b| {
                Digest::new(Algorithm::Sha256, vec![b; 32]).expect("32 bytes is valid sha256")
            }),
            digest_verified: false,
            label: None,
            file_id: inode.map(|inode| FileId { device: 1, inode }),
            link_count: Some(1),
        }
    }

    #[test]
    fn artifacts_without_digest_and_colliding_size_need_hashing() {
        let artifacts = [
            artifact("/a", 100, None, None),    // size collides with /b -> hash
            artifact("/b", 100, Some(1), None), // has digest -> skip
            artifact("/c", 999, None, None),    // unique size -> skip
        ];
        assert_eq!(indices_needing_digest(&artifacts), vec![0]);
    }

    #[test]
    fn equal_digests_form_group_and_unique_digests_do_not() {
        let artifacts = [
            artifact("/a", 100, Some(1), None),
            artifact("/b", 100, Some(1), None),
            artifact("/c", 100, Some(2), None),
        ];
        let groups = duplicate_groups(&artifacts);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members, vec![0, 1]);
        assert_eq!(groups[0].reclaimable, 100);
    }

    #[test]
    fn hardlinked_members_count_as_one_copy() {
        let artifacts = [
            artifact("/a", 100, Some(1), Some(42)),
            artifact("/b", 100, Some(1), Some(42)), // hardlink of /a
            artifact("/c", 100, Some(1), Some(43)),
        ];
        let groups = duplicate_groups(&artifacts);
        assert_eq!(groups[0].reclaimable, 100); // 2 distinct copies -> free 1
    }

    #[test]
    fn groups_sort_by_reclaimable_descending() {
        let artifacts = [
            artifact("/a", 10, Some(1), None),
            artifact("/b", 10, Some(1), None),
            artifact("/big1", 500, Some(2), None),
            artifact("/big2", 500, Some(2), None),
        ];
        let groups = duplicate_groups(&artifacts);
        assert_eq!(groups[0].size, 500);
    }

    #[test]
    fn summary_adds_totals_and_reclaimable() {
        let artifacts = [
            artifact("/a", 100, Some(1), None),
            artifact("/b", 100, Some(1), None),
            artifact("/c", 50, Some(2), None),
        ];
        let groups = duplicate_groups(&artifacts);
        let summary = summarize(&artifacts, &groups);
        assert_eq!(summary.artifact_count, 3);
        assert_eq!(summary.total_bytes, 250);
        assert_eq!(summary.reclaimable_bytes, 100);
    }
}
