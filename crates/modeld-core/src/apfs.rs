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
//! Measured on this project's dev machine: cloning a 44 MB blob allocated 0 blocks.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
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
