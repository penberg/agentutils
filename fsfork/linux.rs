//! Linux: a directory is cloned with `cp --reflink`.

use std::{ffi::OsString, fs, io, path::Path, process::Command};

/// How many entries one `cp` is given, to keep within the limit on arguments.
const ENTRIES_PER_CP: usize = 256;

/// Clones the directory tree at `from` to `to`, which must not exist yet.
/// The clone shares its blocks with the original until either is written, on
/// file systems that can share them (Btrfs, XFS, and others), and is a copy on
/// the rest.
///
/// `to` is made first, so that it fails if `to` exists, as `cp` would copy
/// into it instead.
pub fn clone(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    let out = Command::new("cp")
        .args(["-a", "--reflink=auto", "--no-target-directory", "--"])
        .arg(from)
        .arg(to)
        .output();
    let failed = match out {
        Ok(out) if out.status.success() => return Ok(()),
        Ok(out) => io::Error::other(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Err(e) => e,
    };
    let _ = fs::remove_dir_all(to);
    Err(failed)
}

/// Clones `names`, entries of the directory `from`, into the directory `to`,
/// as `clone` does.
pub fn clone_entries(from: &Path, names: &[OsString], to: &Path) -> io::Result<()> {
    for names in names.chunks(ENTRIES_PER_CP) {
        let out = Command::new("cp")
            .args(["-a", "--reflink=auto", "--target-directory"])
            .arg(to)
            .arg("--")
            .args(names.iter().map(|name| from.join(name)))
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
    }
    Ok(())
}
