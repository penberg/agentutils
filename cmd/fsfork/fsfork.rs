//! `fsfork` forks a directory: it makes a copy-on-write clone of it, the
//! fork, and another, the base, which keeps what the directory was when it
//! was forked, for `fsdiff` to compare the fork with.

use std::{
    io::{self, Write},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    process::ExitCode,
};

use argh::FromArgs;
use fsfork::Contents;

/// Fork a directory: make a copy-on-write clone of it, and print where it is.
#[derive(FromArgs)]
struct Args {
    /// leave out what .gitignore files ignore, unless it is tracked
    #[argh(switch)]
    skip_ignored: bool,

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
    let contents = if args.skip_ignored {
        Contents::NotIgnored
    } else {
        Contents::Everything
    };
    match &args.paths[..] {
        [] => fsfork::fork(Path::new("."), contents),
        [dir] => fsfork::fork(dir, contents),
        [dir, fork] => fsfork::fork_to(dir, fork, contents),
        _ => Err(io::Error::other(
            "too many arguments: a directory, and a fork",
        )),
    }
}
