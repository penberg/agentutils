//! Forks of directories: a fork is a copy-on-write clone of a directory,
//! and has a base next to it, another clone, which keeps what the directory
//! was when it was forked, for `fsdiff` to compare the fork with.

#[cfg(target_os = "macos")]
mod darwin;
mod ignored;
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
use darwin as os;
#[cfg(target_os = "linux")]
use linux as os;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("fsfork runs only on macOS and Linux for now");

use std::{
    ffi::OsString,
    fs, io,
    path::{self, Path, PathBuf},
};

use ignored::Ignored;

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
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
        return Err(io::Error::other("cannot fork /"));
    };
    for n in 1u64.. {
        let mut file_name = OsString::from(name);
        file_name.push(format!(".{n}"));
        let fork = parent.join(file_name);
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

/// The base of `fork`: `.<name>.base` next to it.
pub fn base_of(fork: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(fork.file_name().unwrap_or_default());
    name.push(".base");
    fork.with_file_name(name)
}

/// Forks `dir` to `fork`: clones it to the fork's base, then the base to the
/// fork, so that the two are the same whatever changes in `dir` meanwhile.
fn clone(dir: &Path, fork: &Path, contents: Contents) -> io::Result<()> {
    if fork.starts_with(dir) {
        return Err(io::Error::other(format!(
            "{}: a fork cannot be inside the directory it is a fork of",
            fork.display()
        )));
    }
    let base = base_of(fork);
    match contents {
        Contents::Everything => os::clone(dir, &base),
        Contents::NotIgnored => clone_not_ignored(dir, &base),
    }
    .map_err(|e| at(&base, e))?;
    if let Err(e) = os::clone(&base, fork) {
        let _ = fs::remove_dir_all(&base);
        return Err(at(fork, e));
    }
    Ok(())
}

/// Clones `dir` to `to`, which must not exist, without what is ignored in it,
/// and leaves nothing if it fails.
fn clone_not_ignored(dir: &Path, to: &Path) -> io::Result<()> {
    let ignored = ignored::ignored(dir)?;
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
        return Err(io::Error::other(format!(
            "{}: not a directory",
            dir.display()
        )));
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
            path.display()
        )));
    };
    Ok(parent.canonicalize().map_err(|e| at(parent, e))?.join(name))
}

/// `e`, saying it is about `path`.
fn at(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
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
