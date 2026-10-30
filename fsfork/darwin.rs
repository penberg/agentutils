//! macOS: a directory is cloned with `clonefile`.

use std::{
    ffi::{CString, OsString},
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
    },
    path::Path,
};

/// Copies symbolic links rather than what they point to.
const CLONE_NOFOLLOW: u32 = 0x0001;

/// Clones the directory tree at `from` to `to`, which must not exist yet.
/// The clone shares its blocks with the original until either is written.
pub fn clone(from: &Path, to: &Path) -> io::Result<()> {
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    if unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), CLONE_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Clones `names`, entries of the directory `from`, into the directory `to`.
///
/// `clonefile` takes these in a directory, but not by themselves: FIFOs are
/// made anew, and sockets and devices left out, as nothing is listening on
/// or behind a clone of them.
pub fn clone_entries(from: &Path, names: &[OsString], to: &Path) -> io::Result<()> {
    for name in names {
        let (from, to) = (from.join(name), to.join(name));
        let meta = from.symlink_metadata()?;
        let kind = meta.file_type();
        if kind.is_fifo() {
            let to = CString::new(to.as_os_str().as_bytes())?;
            if unsafe { libc::mkfifo(to.as_ptr(), meta.mode() as libc::mode_t) } != 0 {
                return Err(io::Error::last_os_error());
            }
        } else if !(kind.is_socket() || kind.is_block_device() || kind.is_char_device()) {
            clone(&from, &to)?;
        }
    }
    Ok(())
}
