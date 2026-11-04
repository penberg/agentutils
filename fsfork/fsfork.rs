//! Forks of directories: a fork is a copy-on-write clone of a directory,
//! and has a base next to it, another clone, which keeps what the directory
//! was when it was forked, for `fsdiff` to compare the fork with.

#[cfg(target_os = "macos")]
mod darwin;
pub mod git;
mod ignored;
#[cfg(target_os = "linux")]
mod linux;
pub mod message;

#[cfg(target_os = "macos")]
use darwin as os;
#[cfg(target_os = "linux")]
use linux as os;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("fsfork runs only on macOS and Linux for now");

use std::{
    ffi::{OsStr, OsString},
    fs, io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{self, Path, PathBuf},
    time::{Duration, SystemTime},
};

use ignored::Ignored;
pub use message::at;
use message::shown;

/// What of a directory a fork of it has.
#[derive(Clone, Copy)]
pub enum Contents {
    /// Everything in the directory.
    Everything,
    /// All but what `.gitignore` files ignore, unless it is in the index.
    NotIgnored,
}

/// Forks `dir` to the first of `<dir>.1`, `<dir>.2`, and so on whose fork
/// and base are both free, and returns where the fork is.
pub fn fork(dir: &Path, contents: Contents) -> io::Result<PathBuf> {
    let dir = directory(dir)?;
    for n in 1u64.. {
        let fork = next_to(&dir, OsStr::new(&n.to_string()))?;
        if fork.symlink_metadata().is_ok() {
            continue;
        }
        // Another fsfork may take the number first, which the clone finds.
        match clone(&dir, &fork, contents) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            result => return result.map(|()| fork),
        }
    }
    unreachable!()
}

/// Forks `dir` to `<dir>.<name>` next to it, which must not exist, and
/// returns where the fork is.
pub fn fork_named(dir: &Path, name: &OsStr, contents: Contents) -> io::Result<PathBuf> {
    if name.is_empty() {
        return Err(io::Error::other("a fork's name cannot be empty"));
    }
    if name.as_bytes().contains(&b'/') {
        return Err(io::Error::other(format!(
            "{}: a fork's name cannot have / in it",
            name.to_string_lossy()
        )));
    }
    let dir = directory(dir)?;
    let fork = next_to(&dir, name)?;
    clone(&dir, &fork, contents)?;
    Ok(fork)
}

/// Forks `dir` to `fork`, which must not exist, and returns where the fork
/// is, made absolute.
pub fn fork_to(dir: &Path, fork: &Path, contents: Contents) -> io::Result<PathBuf> {
    let dir = directory(dir)?;
    let fork = resolve(fork)?;
    clone(&dir, &fork, contents)?;
    Ok(fork)
}

/// Removes `fork` and its base, both even if one cannot be.
pub fn remove(fork: &Path) -> io::Result<()> {
    let base = base_of(fork);
    let removed = fs::remove_dir_all(fork).map_err(|e| at(fork, e));
    let base_removed = fs::remove_dir_all(&base).map_err(|e| at(&base, e));
    removed.and(base_removed)
}

/// A fork of a directory, next to it, or what is left of one.
pub struct Found {
    /// Where the fork is, or was.
    pub fork: PathBuf,
    /// Where its base is.
    pub base: PathBuf,
    /// Whether only the base is left, the fork having been removed without
    /// it.
    pub left_over: bool,
    /// When it was made, roughly: when its base last changed.
    pub made: SystemTime,
}

/// The forks of `dir` next to it, `<dir>.<suffix>` with a base, and the bases
/// left over from forks removed without them, oldest first. Forks made
/// elsewhere, with a path given, are not found.
///
/// When each was made is when its base last changed, which it never does once
/// made, but for attributes the system may add to it later.
pub fn forks(dir: &Path) -> io::Result<Vec<Found>> {
    let dir = directory(dir)?;
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
        return Ok(Vec::new());
    };
    let mut prefix = name.as_bytes().to_vec();
    prefix.push(b'.');
    let mut found = Vec::new();
    for entry in fs::read_dir(parent).map_err(|e| at(parent, e))? {
        let entry = entry.map_err(|e| at(parent, e))?;
        let entry_name = entry.file_name();
        let entry_name = entry_name.as_bytes();
        // A fork is found by its base, `.<dir>.<suffix>.base`, which every
        // fork has, and a directory merely named like one does not.
        let Some(fork_name) = entry_name
            .strip_prefix(b".")
            .and_then(|name| name.strip_suffix(b".base"))
            .filter(|name| name.len() > prefix.len() && name.starts_with(&prefix))
        else {
            continue;
        };
        let base = entry.path();
        let meta = base.symlink_metadata().map_err(|e| at(&base, e))?;
        if !meta.is_dir() {
            continue;
        }
        let fork = parent.join(OsStr::from_bytes(fork_name));
        let fork_is_dir = fork.symlink_metadata().is_ok_and(|m| m.is_dir());
        let made = SystemTime::UNIX_EPOCH
            + Duration::new(
                u64::try_from(meta.ctime()).unwrap_or(0),
                u32::try_from(meta.ctime_nsec()).unwrap_or(0),
            );
        found.push(Found {
            left_over: !fork_is_dir,
            fork,
            base,
            made,
        });
    }
    found.sort_by(|a, b| a.made.cmp(&b.made).then_with(|| a.fork.cmp(&b.fork)));
    Ok(found)
}

/// The base of `fork`: `.<name>.base` next to it.
pub fn base_of(fork: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(fork.file_name().unwrap_or_default());
    name.push(".base");
    fork.with_file_name(name)
}

/// `<dir>.<suffix>`, next to `dir`.
fn next_to(dir: &Path, suffix: &OsStr) -> io::Result<PathBuf> {
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
        return Err(io::Error::other("cannot fork /"));
    };
    let mut file_name = OsString::from(name);
    file_name.push(".");
    file_name.push(suffix);
    Ok(parent.join(file_name))
}

/// Forks `dir` to `fork`: clones it to the fork's base, then the base to the
/// fork, so that the two are the same whatever changes in `dir` meanwhile.
fn clone(dir: &Path, fork: &Path, contents: Contents) -> io::Result<()> {
    if fork.starts_with(dir) {
        return Err(io::Error::other(format!(
            "{}: cannot be inside the directory it forks",
            shown(fork).display()
        )));
    }
    if fork.symlink_metadata().is_ok() {
        return Err(at(fork, io::Error::from_raw_os_error(libc::EEXIST)));
    }
    let base = base_of(fork);
    match contents {
        Contents::Everything => os::clone(dir, &base),
        Contents::NotIgnored => clone_not_ignored(dir, &base),
    }
    .map_err(|e| match e.kind() {
        // A fork removed without its base.
        io::ErrorKind::AlreadyExists => io::Error::new(
            e.kind(),
            format!(
                "{}: its base {} is left over; remove it",
                shown(fork).display(),
                shown(&base).display()
            ),
        ),
        _ => at(&base, e),
    })?;
    if let Err(e) = os::clone(&base, fork) {
        let _ = fs::remove_dir_all(&base);
        return Err(at(fork, e));
    }
    Ok(())
}

/// Clones `dir` to `to`, which must not exist, without what is ignored in it,
/// or with it if that cannot be told, and leaves nothing if it fails.
fn clone_not_ignored(dir: &Path, to: &Path) -> io::Result<()> {
    // A fork with too much in it works, so this is no reason not to make one.
    let ignored = match ignored::ignored(dir) {
        Ok(ignored) => ignored,
        Err(e) => {
            message::say(format!(
                "forking everything, as git cannot tell what is ignored: {}",
                message::describe(&e)
            ));
            return os::clone(dir, to);
        }
    };
    if ignored.paths.is_empty() {
        return os::clone(dir, to);
    }
    fs::create_dir(to)?;
    let cloned = clone_around(dir, to, &ignored);
    if cloned.is_err() {
        let _ = fs::remove_dir_all(to);
    }
    cloned
}

/// Fills `to`, a new directory, with the clone of `dir`, a directory with
/// something ignored in it: only such directories are gone through, and
/// everything else is cloned whole, at once.
fn clone_around(dir: &Path, to: &Path, ignored: &Ignored) -> io::Result<()> {
    let mut whole = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if ignored.paths.contains(&path) {
            continue;
        }
        if ignored.around.contains(&path) {
            let inside = to.join(entry.file_name());
            fs::create_dir(&inside)?;
            clone_around(&path, &inside, ignored)?;
        } else {
            whole.push(entry.file_name());
        }
    }
    os::clone_entries(dir, &whole, to)?;
    let meta = fs::symlink_metadata(dir)?;
    fs::File::open(to)?.set_modified(meta.modified()?)?;
    fs::set_permissions(to, meta.permissions())
}

/// `path`, canonical, if it is a directory.
fn directory(path: &Path) -> io::Result<PathBuf> {
    let dir = path.canonicalize().map_err(|e| at(path, e))?;
    if !dir.is_dir() {
        return Err(at(path, io::Error::from_raw_os_error(libc::ENOTDIR)));
    }
    Ok(dir)
}

/// `path` made absolute, with the directory it is in resolved as
/// `canonicalize` does, but not `path` itself, which need not exist.
fn resolve(path: &Path) -> io::Result<PathBuf> {
    let path = path::absolute(path)?;
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::other(format!(
            "{}: not a place for a fork",
            shown(&path).display()
        )));
    };
    Ok(parent.canonicalize().map_err(|e| at(parent, e))?.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_is_hidden_next_to_the_fork() {
        assert_eq!(
            base_of(Path::new("/src/turso.1")),
            Path::new("/src/.turso.1.base")
        );
    }
}
