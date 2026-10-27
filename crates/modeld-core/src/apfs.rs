//! Thin safe wrappers over APFS-specific syscalls (macOS only).
//!
//! Two primitives carry the whole dedupe design:
//!
//! - [`clone_file`] — `clonefile(2)`: copy-on-write clone sharing extents with the
//!   source. Same disk savings as a hardlink, but an independent inode, so a
//!   provider mutating its copy can never corrupt the canonical bytes.
//! - [`swap_files`] — `renamex_np(2)` with `RENAME_SWAP`: atomically exchanges two
//!   paths. The replaced file's original bytes survive under the temp name as an
//!   instant rollback until explicitly released.
//!
//! [`shares_extents`] (`fcntl(F_LOG2PHYS_EXT)`) answers the inverse question —
//! are two files already clones? — for files modeld did not record cloning.
//! [`private_size`] (`getattrlist(ATTR_CMNEXT_PRIVATESIZE)`) says how many bytes
//! deleting one file would free: the blocks no clone shares.
//!
//! Measured on this project's dev machine: cloning a 44 MB blob allocated 0 blocks.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Clones `src` to `dst` via `clonefile(2)`; `dst` must not exist.
///
/// # Errors
/// The underlying OS error — notably cross-volume (`EXDEV`), non-APFS filesystem
/// (`ENOTSUP`), or `dst` already existing (`EEXIST`).
pub fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    let src_c = cstring(src)?;
    let dst_c = cstring(dst)?;
    // SAFETY: both pointers are valid NUL-terminated C strings that outlive the
    // call; clonefile reads them synchronously and does not retain them.
    let rc = unsafe { libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Atomically exchanges two existing paths via `renamex_np(2)` + `RENAME_SWAP`.
///
/// Both paths must exist on the same volume. After return, each path names the
/// other's former file. There is no intermediate state visible to other processes.
///
/// # Errors
/// The underlying OS error — notably cross-volume (`EXDEV`) or a missing path
/// (`ENOENT`).
pub fn swap_files(a: &Path, b: &Path) -> io::Result<()> {
    let a_c = cstring(a)?;
    let b_c = cstring(b)?;
    // SAFETY: both pointers are valid NUL-terminated C strings that outlive the
    // call; renamex_np reads them synchronously and does not retain them.
    let rc = unsafe { libc::renamex_np(a_c.as_ptr(), b_c.as_ptr(), libc::RENAME_SWAP) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Whether `a` and `b` are backed by the same physical blocks (an APFS clone pair).
///
/// Maps evenly spaced file offsets to device offsets with
/// `fcntl(F_LOG2PHYS_EXT)` and compares them. Clones map every offset to the
/// same block until copy-on-write diverges them; independent copies never do.
/// Sampled, so a clone diverged only between samples still reads as shared.
///
/// # Errors
/// Opening either file or the mapping call failed (e.g. a filesystem without
/// physical mapping support).
pub fn shares_extents(a: &Path, b: &Path) -> io::Result<bool> {
    const SAMPLES: u64 = 16;
    let (a, b) = (File::open(a)?, File::open(b)?);
    let (a_meta, b_meta) = (a.metadata()?, b.metadata()?);
    // Device offsets are only comparable within one volume.
    let len = a_meta.len();
    if len == 0 || len != b_meta.len() || a_meta.dev() != b_meta.dev() {
        return Ok(false);
    }
    for sample in 0..SAMPLES {
        let offset = (len - 1) * sample / (SAMPLES - 1);
        if device_offset(&a, offset)? != device_offset(&b, offset)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Bytes only `path` holds: what deleting it alone would free.
///
/// Blocks a clone (or the file a clone was made from) still shares are not
/// counted. Clone IDs (`ATTR_CMNEXT_CLONEID`) cannot replace this: a clone
/// gets a new ID as soon as one block diverges, while it still shares the
/// rest.
///
/// # Errors
/// The path cannot be read, or the filesystem does not report private sizes
/// (only APFS does).
pub fn private_size(path: &Path) -> io::Result<u64> {
    /// The reply layout: attributes arrive in bit order after the header.
    #[repr(C, packed(4))]
    struct Reply {
        length: u32,
        returned: libc::attribute_set_t,
        private_size: libc::off_t,
    }

    let path_c = cstring(path)?;
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: libc::ATTR_CMNEXT_PRIVATESIZE,
    };
    let mut reply = Reply {
        length: 0,
        returned: libc::attribute_set_t {
            commonattr: 0,
            volattr: 0,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        },
        private_size: 0,
    };
    // SAFETY: `path_c` is a valid NUL-terminated C string, `request` a valid
    // attrlist, and `reply` a writable buffer of exactly the size passed;
    // getattrlist writes at most that many bytes and keeps no pointers.
    let rc = unsafe {
        libc::getattrlist(
            path_c.as_ptr(),
            (&raw mut request).cast(),
            (&raw mut reply).cast(),
            std::mem::size_of::<Reply>(),
            libc::FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let returned = reply.returned.forkattr;
    if returned & libc::ATTR_CMNEXT_PRIVATESIZE == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem reports no private size",
        ));
    }
    let size = reply.private_size;
    u64::try_from(size)
        .map_err(|_negative| io::Error::new(io::ErrorKind::InvalidData, "negative private size"))
}

/// Physical device offset backing byte `offset` of `file`.
fn device_offset(file: &File, offset: u64) -> io::Result<libc::off_t> {
    let mut mapping = libc::log2phys {
        l2p_flags: 0,
        l2p_contigbytes: 1,
        l2p_devoffset: libc::off_t::try_from(offset)
            .map_err(|_overflow| io::Error::new(io::ErrorKind::InvalidInput, "offset too large"))?,
    };
    // SAFETY: the descriptor is open for the lifetime of `file`, and `mapping`
    // is a valid, exclusively borrowed `log2phys` that F_LOG2PHYS_EXT reads
    // (file offset, length) and overwrites (device offset) synchronously.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_LOG2PHYS_EXT, &raw mut mapping) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(mapping.l2p_devoffset)
}

fn cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_nul| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_file_produces_identical_content() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::write(&src, b"model bytes").expect("write src");

        clone_file(&src, &dst).expect("clonefile");

        assert_eq!(std::fs::read(&dst).expect("read dst"), b"model bytes");
    }

    #[test]
    fn clone_file_refuses_existing_destination() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::write(&src, b"a").expect("write src");
        std::fs::write(&dst, b"b").expect("write dst");

        let error = clone_file(&src, &dst).expect_err("must refuse");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }

    /// Writes a multi-block file and forces allocation so blocks have addresses.
    fn allocated_file(path: &Path) {
        let content: Vec<u8> = (0..256 * 1024u32).map(|i| (i % 251) as u8).collect();
        let mut file = std::fs::File::create(path).expect("create");
        std::io::Write::write_all(&mut file, &content).expect("write");
        file.sync_all().expect("sync");
    }

    #[test]
    fn clone_shares_extents_with_its_source() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        allocated_file(&src);

        clone_file(&src, &dst).expect("clonefile");

        assert!(shares_extents(&src, &dst).expect("map"));
    }

    #[test]
    fn independent_copy_shares_no_extents() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        allocated_file(&a);
        allocated_file(&b);

        assert!(!shares_extents(&a, &b).expect("map"));
    }

    #[test]
    fn clone_stops_sharing_where_it_was_rewritten() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        allocated_file(&src);
        clone_file(&src, &dst).expect("clonefile");

        let content = std::fs::read(&src).expect("read");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&dst)
            .expect("open clone");
        std::io::Write::write_all(&mut file, &content).expect("rewrite same bytes");
        file.sync_all().expect("sync");

        assert!(!shares_extents(&src, &dst).expect("map"));
    }

    #[test]
    fn private_size_drops_to_zero_when_cloned_and_grows_where_rewritten() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        allocated_file(&src);
        let alone = private_size(&src).expect("private size");

        clone_file(&src, &dst).expect("clonefile");
        let shared = (
            private_size(&src).expect("src"),
            private_size(&dst).expect("dst"),
        );
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&dst)
            .expect("open clone");
        std::io::Write::write_all(&mut file, &vec![1u8; 64 * 1024]).expect("rewrite start");
        file.sync_all().expect("sync");
        let diverged = private_size(&dst).expect("dst after rewrite");

        assert!(alone >= 256 * 1024, "{alone}");
        assert_eq!(shared, (0, 0));
        assert!(diverged >= 64 * 1024 && diverged < alone, "{diverged}");
    }

    #[test]
    fn swap_files_exchanges_contents_atomically() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"first").expect("write a");
        std::fs::write(&b, b"second").expect("write b");

        swap_files(&a, &b).expect("swap");

        assert_eq!(std::fs::read(&a).expect("read a"), b"second");
        assert_eq!(std::fs::read(&b).expect("read b"), b"first");
    }
}
