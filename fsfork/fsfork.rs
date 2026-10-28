//! `fsfork` forks a directory: it makes a copy-on-write clone of it, the
//! fork, and another, the base, which keeps what the directory was when it
//! was forked, for `fsdiff` to compare the fork with.

#[cfg(target_os = "macos")]
mod darwin;
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
    fs,
    io::{self, Write},
    os::unix::ffi::OsStringExt,
    path::{self, Path, PathBuf},
    process::ExitCode,
};

use argh::FromArgs;

/// Fork a directory: make a copy-on-write clone of it, and print where it is.
#[derive(FromArgs)]
struct Args {
    /// the directory to fork, the working directory if none; then where to
    /// put the fork, which must not exist, or next to the directory, named
    /// after it and the first number not taken, if not given
    #[argh(positional, arg_name = "dir")]
    paths: Vec<PathBuf>,
}

fn main() -> ExitCode {
    let args: Args = argh::from_env();
    match fsfork(args) {
        Ok(fork) => {
            let mut line = fork.into_os_string().into_vec();
            line.push(b'\n');
            if let Err(e) = io::stdout().write_all(&line) {
                eprintln!("fsfork: {e}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("fsfork: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Forks the directory `args` give, and returns where the fork is.
fn fsfork(args: Args) -> io::Result<PathBuf> {
    let (dir, fork) = match &args.paths[..] {
        [] => (Path::new("."), None),
        [dir] => (dir.as_path(), None),
        [dir, fork] => (dir.as_path(), Some(fork)),
        _ => {
            return Err(io::Error::other(
                "too many arguments: a directory, and a fork",
            ));
        }
    };
    let dir = dir.canonicalize().map_err(|e| at(dir, e))?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "{}: not a directory",
            dir.display()
        )));
    }
    match fork {
        Some(fork) => {
            let fork = resolve(fork)?;
            fork_to(&dir, &fork)?;
            Ok(fork)
        }
        None => fork_next(&dir),
    }
}

/// Forks `dir` to the first of `<dir>.1`, `<dir>.2`, and so on whose fork
/// and base are both free, and returns where the fork is.
fn fork_next(dir: &Path) -> io::Result<PathBuf> {
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
        match fork_to(dir, &fork) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            result => return result.map(|()| fork),
        }
    }
    unreachable!()
}

/// Forks `dir` to `fork`: clones it to the fork's base, then the base to the
/// fork, so that the two are the same whatever changes in `dir` meanwhile.
fn fork_to(dir: &Path, fork: &Path) -> io::Result<()> {
    if fork.starts_with(dir) {
        return Err(io::Error::other(format!(
            "{}: a fork cannot be inside the directory it is a fork of",
            fork.display()
        )));
    }
    let base = base_of(fork);
    os::clone(dir, &base).map_err(|e| at(&base, e))?;
    if let Err(e) = os::clone(&base, fork) {
        let _ = fs::remove_dir_all(&base);
        return Err(at(fork, e));
    }
    Ok(())
}

/// The base of `fork`: `.<name>.base` next to it.
fn base_of(fork: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(fork.file_name().unwrap_or_default());
    name.push(".base");
    fork.with_file_name(name)
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
