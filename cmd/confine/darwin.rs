//! macOS: a command is confined with Seatbelt.

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

/// A command that runs `command` under Seatbelt: it may write only under
/// `writable` and to devices, and may not list, read, or write anything
/// under `hidden` except `dir`, which it may read and write. Everything
/// else, the GPU and the network included, is allowed.
///
/// The paths are passed as parameters rather than written into the profile,
/// so they need no quoting, and must be canonical, as Seatbelt matches the
/// path a file is opened at after symbolic links are resolved.
pub fn confine(
    command: &[String],
    dir: &Path,
    writable: &[PathBuf],
    hidden: &[PathBuf],
) -> io::Result<Command> {
    let mut profile = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    let mut params = Vec::new();
    profile.push_str("(allow file-write* (subpath \"/dev\")");
    for (i, path) in writable.iter().enumerate() {
        profile.push_str(&format!(" (subpath (param \"W{i}\"))"));
        params.push(format!("W{i}={}", path.display()));
    }
    profile.push_str(")\n");
    // Later rules win, so the hidden paths are denied after the writable
    // ones, and the directory, which may be under one of them, is allowed
    // last. Their metadata stays readable, and so do the directories above
    // the directory, which may be hidden, but which resolving a path in the
    // directory stats and `getcwd` lists.
    if !hidden.is_empty() {
        profile.push_str("(deny file-read-data file-write*");
        for (i, path) in hidden.iter().enumerate() {
            profile.push_str(&format!(" (subpath (param \"H{i}\"))"));
            params.push(format!("H{i}={}", path.display()));
        }
        profile.push_str(")\n(allow file-read-data");
        for (i, path) in dir.ancestors().skip(1).enumerate() {
            profile.push_str(&format!(" (literal (param \"A{i}\"))"));
            params.push(format!("A{i}={}", path.display()));
        }
        profile.push_str(")\n");
    }
    // A rule for `file-read-data` wins over one for `file-read*` whatever
    // their order, so it is allowed by name.
    profile.push_str("(allow file-read* file-read-data file-write* (subpath (param \"DIR\")))\n");
    params.push(format!("DIR={}", dir.display()));
    let mut sandbox = Command::new("/usr/bin/sandbox-exec");
    sandbox.arg("-p").arg(profile);
    for param in params {
        sandbox.arg("-D").arg(param);
    }
    sandbox.arg("--").args(command);
    Ok(sandbox)
}
