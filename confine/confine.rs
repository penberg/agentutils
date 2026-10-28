//! `confine` runs a command confined to a directory: the command may write
//! there, to temporary directories, and to caches, but nowhere else, and may
//! not read secrets such as SSH keys.

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
use darwin as os;
#[cfg(target_os = "linux")]
use linux as os;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("confine runs only on macOS and Linux for now");

use std::{
    env, io,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::ExitCode,
};

use argh::FromArgs;

/// Directories under the home directory that the command may write to, so
/// that builds and package managers keep working.
const CACHES: &[&str] = &[".cargo", ".rustup", ".cache", ".npm", "Library/Caches"];

/// Directories under the home directory that the command may not read.
const SECRETS: &[&str] = &[".ssh", ".aws", ".gnupg", ".config/gh"];

/// Run a command that may write only to a directory, temporary directories
/// and caches, and may not read secrets.
#[derive(FromArgs)]
struct Args {
    /// another path the command may write to; can be repeated
    #[argh(option, short = 'w')]
    write: Vec<PathBuf>,

    /// the directory the command runs in and may write to
    #[argh(positional)]
    dir: PathBuf,

    /// the command to run and its arguments; the shell if none
    #[argh(positional, greedy)]
    command: Vec<String>,
}

fn main() -> ExitCode {
    let args: Args = argh::from_env();
    let e = match confine(&args) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("confine: {e}");
            return ExitCode::FAILURE;
        }
    };
    // As env(1) does: 127 if the command was not found, 126 if it could not
    // be run.
    eprintln!("confine: {}: {e}", args.command.first().map_or("", |c| c));
    if e.kind() == io::ErrorKind::NotFound {
        ExitCode::from(127)
    } else {
        ExitCode::from(126)
    }
}

/// Runs the command `args` give in its directory, confined to it, in place of
/// this process. Returns only if it cannot: with why the command could not be
/// run, or with an error from before it was tried.
fn confine(args: &Args) -> io::Result<io::Error> {
    let dir = args.dir.canonicalize().map_err(|e| at(&args.dir, e))?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "{}: not a directory",
            dir.display()
        )));
    }
    let home = dirs::home_dir().ok_or_else(|| io::Error::other("no home directory"))?;
    let mut writable = vec![PathBuf::from("/tmp"), env::temp_dir()];
    writable.extend(CACHES.iter().map(|cache| home.join(cache)));
    let mut writable: Vec<PathBuf> = writable
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect();
    for path in &args.write {
        writable.push(path.canonicalize().map_err(|e| at(path, e))?);
    }
    let hidden: Vec<PathBuf> = SECRETS
        .iter()
        .map(|secret| home.join(secret))
        .filter_map(|path| path.canonicalize().ok())
        .collect();
    let command = if args.command.is_empty() {
        vec![env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())]
    } else {
        args.command.clone()
    };
    let mut confined = os::confine(&command, &dir, &writable, &hidden)?;
    // Claude Code would otherwise ask whether to trust every new directory,
    // and accepting would write to `~/.claude.json`, which it may not.
    confined.current_dir(&dir).env("CLAUDE_CODE_SANDBOXED", "1");
    Ok(confined.exec())
}

/// `e`, saying it is about `path`.
fn at(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}
