//! `fsdiff` shows what changed in a fork since it was made, as a patch: the
//! difference between the fork and its base, which `fsfork` made next to it.

mod git;

use std::{
    env,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::ExitCode,
};

use argh::FromArgs;

/// Show what changed in a fork since it was made, or between two
/// directories, as a patch.
#[derive(FromArgs)]
struct Args {
    /// show only which files changed, and how much
    #[argh(switch)]
    stat: bool,

    /// the fork, the working directory if none; or two directories, the
    /// second to compare with the first
    #[argh(positional, arg_name = "dir")]
    paths: Vec<PathBuf>,
}

/// As diff(1) does, `fsdiff` exits with 0 if there is no difference, 1 if
/// there is, and 2 if something went wrong.
const TROUBLE: u8 = 2;

fn main() -> ExitCode {
    let args = match parse() {
        Ok(args) => args,
        Err(code) => return code,
    };
    // The terminal's signals reach git too, which exits; `fsdiff` outlives
    // it to remove its repository.
    for signal in git::SIGNALS {
        unsafe { libc::signal(signal, libc::SIG_IGN) };
    }
    match fsdiff(&args) {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => ExitCode::from(1),
        Err(e) => {
            eprintln!("fsdiff: {e}");
            ExitCode::from(TROUBLE)
        }
    }
}

/// The arguments `fsdiff` was given, or the code to exit with if they were
/// not right, or asked for help.
fn parse() -> Result<Args, ExitCode> {
    let strings: Vec<String> = env::args().collect();
    let command = Path::new(&strings[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("fsdiff");
    let strs: Vec<&str> = strings[1..].iter().map(String::as_str).collect();
    Args::from_args(&[command], &strs).map_err(|exit| match exit.status {
        Ok(()) => {
            print!("{}", exit.output);
            ExitCode::SUCCESS
        }
        Err(()) => {
            eprint!("{}", exit.output);
            ExitCode::from(TROUBLE)
        }
    })
}

/// Shows the difference `args` ask for, and returns whether there is one.
fn fsdiff(args: &Args) -> io::Result<bool> {
    let (old, new) = match &args.paths[..] {
        [old, new] => (directory(old)?, directory(new)?),
        [] | [_] => {
            let fork = args.paths.first().map_or(Path::new("."), PathBuf::as_path);
            let fork = directory(fork)?;
            let base = base_of(&fork);
            if !base.is_dir() {
                return Err(io::Error::other(format!(
                    "{}: no base at {}; is it a fork?",
                    fork.display(),
                    base.display()
                )));
            }
            (base, fork)
        }
        _ => {
            return Err(io::Error::other(
                "too many arguments: a fork, or two directories",
            ));
        }
    };
    let repo = git::Repo::new()?;
    let (before, after) = repo.record(&old, &new)?;
    repo.diff(&before, &after, args.stat)
}

/// `path`, canonical, if it is a directory.
fn directory(path: &Path) -> io::Result<PathBuf> {
    let dir = path
        .canonicalize()
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "{}: not a directory",
            dir.display()
        )));
    }
    Ok(dir)
}

/// The base of `fork`: `.<name>.base` next to it.
fn base_of(fork: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(fork.file_name().unwrap_or_default());
    name.push(".base");
    fork.with_file_name(name)
}
