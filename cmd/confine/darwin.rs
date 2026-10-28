//! macOS: a command is confined with Seatbelt.

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

/// A command that runs `command` under Seatbelt: it may write only under
/// `writable` and to devices, and may not list or read anything under
/// `hidden` but what is under `readable` or `writable`, and `dir`, which it
/// may read and write. Everything else, the GPU and the network included, is
/// allowed.
///
/// The paths are passed as parameters rather than written into the profile,
/// so they need no quoting, and must be canonical, as Seatbelt matches the
/// path a file is opened at after symbolic links are resolved.
pub fn confine(
    command: &[String],
    dir: &Path,
    writable: &[PathBuf],
    readable: &[PathBuf],
    hidden: &[PathBuf],
) -> io::Result<Command> {
    let mut profile = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    let mut params = Vec::new();
    let writable = filters(&mut params, "W", "subpath", writable);
    profile.push_str(&format!(
        "(allow file-write* (subpath \"/dev\"){writable})\n"
    ));
    // Later rules win, so the hidden paths are denied after the writable
    // ones are allowed, what may be read under them is allowed after, and
    // the directory, which may be under them too, last. Their metadata stays
    // readable, and so do the directories above the directory, which
    // resolving a path in the directory stats and `getcwd` lists.
    if !hidden.is_empty() {
        let hidden = filters(&mut params, "H", "subpath", hidden);
        profile.push_str(&format!("(deny file-read-data{hidden})\n"));
        let ancestors: Vec<PathBuf> = dir.ancestors().skip(1).map(Path::to_path_buf).collect();
        let ancestors = filters(&mut params, "A", "literal", &ancestors);
        let readable = filters(&mut params, "R", "subpath", readable);
        let allowed = format!("{ancestors}{readable}{writable}");
        if !allowed.is_empty() {
            profile.push_str(&format!("(allow file-read-data{allowed})\n"));
        }
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

/// The filters, each starting with a space, that match `paths` with `kind`,
/// `subpath` or `literal`, given as parameters named `prefix` and their
/// index, which are added to `params`.
fn filters(params: &mut Vec<String>, prefix: &str, kind: &str, paths: &[PathBuf]) -> String {
    let mut filters = String::new();
    for (i, path) in paths.iter().enumerate() {
        filters.push_str(&format!(" ({kind} (param \"{prefix}{i}\"))"));
        params.push(format!("{prefix}{i}={}", path.display()));
    }
    filters
}
