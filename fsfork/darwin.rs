//! macOS: a directory is cloned with `clonefile`.

use std::{ffi::CString, io, os::unix::ffi::OsStrExt, path::Path};

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
