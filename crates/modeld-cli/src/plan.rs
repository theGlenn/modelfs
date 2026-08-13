//! Builds a consolidation plan from duplicate groups (macOS only).

use modeld_core::consolidate::Replacement;
use modeld_core::dedup::DuplicateGroup;
use modeld_core::{Algorithm, Artifact};

/// Turns duplicate groups into concrete replacements, applying safety policy.
///
/// Policy: only sha256-identified groups qualify; the first member is kept as
/// canonical; members already hardlinked to the canonical or on another volume are
/// excluded (clonefile cannot cross volumes; hardlinks already share storage).
pub fn build(artifacts: &[Artifact], groups: &[DuplicateGroup]) -> Vec<Replacement> {
    let mut replacements = Vec::new();
    for group in groups {
        if group.digest.algorithm() != Algorithm::Sha256 {
            continue;
        }
        let canonical = &artifacts[group.members[0]];
        for &member in &group.members[1..] {
            let victim = &artifacts[member];
            let same_inode = canonical.file_id.is_some() && canonical.file_id == victim.file_id;
            let same_device = match (canonical.file_id, victim.file_id) {
                (Some(a), Some(b)) => a.device == b.device,
                _ => false,
            };
            if same_inode || !same_device {
                continue;
            }
            replacements.push(Replacement {
                canonical: canonical.path.clone(),
                victim: victim.path.clone(),
                digest: group.digest.clone(),
                size: group.size,
            });
        }
    }
    replacements
}

#[cfg(test)]
mod tests {
    use super::*;
    use modeld_core::dedup::duplicate_groups;
    use modeld_core::{Digest, FileId, ProviderKind};
    use std::path::PathBuf;

    fn artifact(path: &str, digest_byte: u8, device: u64, inode: u64) -> Artifact {
        Artifact {
            path: PathBuf::from(path),
            size: 100,
            provider: ProviderKind::Manual,
            format: None,
            digest: Some(
                Digest::new(Algorithm::Sha256, vec![digest_byte; 32]).expect("valid sha256 len"),
            ),
            digest_verified: true,
            label: None,
            file_id: Some(FileId { device, inode }),
        }
    }

    #[test]
    fn first_member_is_canonical_and_rest_become_victims() {
        let artifacts = [artifact("/keep", 1, 1, 10), artifact("/replace", 1, 1, 11)];
        let plan = build(&artifacts, &duplicate_groups(&artifacts));
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].canonical, PathBuf::from("/keep"));
        assert_eq!(plan[0].victim, PathBuf::from("/replace"));
    }

    #[test]
    fn excludes_hardlinks_and_cross_volume_members() {
        let artifacts = [
            artifact("/keep", 1, 1, 10),
            artifact("/hardlink-of-keep", 1, 1, 10),
            artifact("/other-volume", 1, 2, 10),
        ];
        let plan = build(&artifacts, &duplicate_groups(&artifacts));
        assert!(plan.is_empty());
    }
}
