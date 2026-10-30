//! `fsfork` forks a directory: it makes a copy-on-write clone of it, the
//! fork, and another, the base, which keeps what the directory was when it
//! was forked, for `fsdiff` to compare the fork with.

use std::{
    env,
    io::{self, IsTerminal, Write},
    os::unix::{ffi::OsStringExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use argh::FromArgs;
use fsfork::Contents;

/// Fork a directory: make a copy-on-write clone of it, and start a shell there
/// or, if not on a terminal, print where it is.
#[derive(FromArgs)]
struct Args {
    /// print where the fork is, rather than start a shell there
    #[argh(switch)]
    print: bool,

    /// fork what .gitignore files ignore too, which is otherwise left out
    #[argh(switch)]
    no_ignore: bool,

    /// the directory to fork, the working directory if none; then where to
    /// put the fork, which must not exist, or next to the directory, named
    /// after it and the first number not taken, if not given
    #[argh(positional, arg_name = "dir")]
    paths: Vec<PathBuf>,
}

fn main() -> ExitCode {
    let args: Args = argh::from_env();
    // A shell needs both: with stdout captured, as in `cd $(fsfork)`, the
    // path is wanted, and with stdin not a terminal, a shell would run what
    // it reads from there as commands.
    let shell = !args.print && io::stdin().is_terminal() && io::stdout().is_terminal();
    let forked = fsfork(args).and_then(|fork| {
        if shell {
            start_shell(&fork)
        } else {
            print(fork)
        }
    });
    match forked {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fsfork: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Prints where `fork` is.
fn print(fork: PathBuf) -> io::Result<()> {
    let mut line = fork.into_os_string().into_vec();
    line.push(b'\n');
    io::stdout().write_all(&line)
}

/// Runs the shell in `fork`, in place of this process. If it cannot be run,
/// removes the fork, and returns why.
fn start_shell(fork: &Path) -> io::Result<()> {
    eprintln!("fsfork: forked to {}, exit to come back", fork.display());
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let e = Command::new(&shell).current_dir(fork).exec();
    if let Err(e) = fsfork::remove(fork) {
        eprintln!("fsfork: {e}");
    }
    Err(io::Error::other(format!("{shell}: {e}")))
}

/// Forks the directory `args` give, and returns where the fork is.
fn fsfork(args: Args) -> io::Result<PathBuf> {
    let contents = if args.no_ignore {
        Contents::Everything
    } else {
        Contents::NotIgnored
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
