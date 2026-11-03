//! `fsfork` forks a directory: it makes a copy-on-write clone of it, the
//! fork, and another, the base, which keeps what the directory was when it
//! was forked, for `fsdiff` to compare the fork with.

use std::{
    env,
    ffi::OsStr,
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

    /// name the fork <dir>.<name>, next to the directory, rather than after
    /// the first number not taken
    #[argh(option, short = 'n', arg_name = "name")]
    name: Option<String>,

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
    eprintln!("fsfork: forked to {}", fork.display());
    let fork = fork.canonicalize().unwrap_or_else(|_| fork.to_path_buf());
    report_cwd(&fork);
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let e = Command::new(&shell)
        .current_dir(&fork)
        .env("PWD", &fork)
        .exec();
    if let Err(e) = fsfork::remove(&fork) {
        eprintln!("fsfork: {e}");
    }
    Err(io::Error::other(format!("{shell}: {e}")))
}

/// Tells the terminal, on stdout, that its working directory is `dir`, with
/// OSC 7, as a shell does at its prompt, so that a new tab or window opens
/// there. The terminal may have given only the shell it started a way to say
/// so, as Ghostty does, so the shell started here may not, and the terminal
/// would otherwise go by what the last one said: the directory forked. The
/// shell `fsfork` was started from says again where it is once this one
/// exits.
fn report_cwd(dir: &Path) {
    let mut osc = format!("\x1b]7;file://{}", hostname()).into_bytes();
    for &b in dir.as_os_str().as_encoded_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            osc.push(b);
        } else {
            osc.extend(format!("%{b:02X}").bytes());
        }
    }
    osc.push(b'\x07');
    // A terminal that cannot be told is no reason not to start the shell.
    let mut stdout = io::stdout();
    let _ = stdout.write_all(&osc).and_then(|()| stdout.flush());
}

/// The host's name, or none if it has none to give.
fn hostname() -> String {
    let mut name = [0u8; 256];
    let got = unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) };
    if got != 0 {
        return String::new();
    }
    let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    String::from_utf8_lossy(&name[..len]).into_owned()
}

/// Forks the directory `args` give, and returns where the fork is.
fn fsfork(args: Args) -> io::Result<PathBuf> {
    let contents = if args.no_ignore {
        Contents::Everything
    } else {
        Contents::NotIgnored
    };
    match (&args.name, &args.paths[..]) {
        (None, []) => fsfork::fork(Path::new("."), contents),
        (None, [dir]) => fsfork::fork(dir, contents),
        (None, [dir, fork]) => fsfork::fork_to(dir, fork, contents),
        (Some(name), []) => fsfork::fork_named(Path::new("."), OsStr::new(name), contents),
        (Some(name), [dir]) => fsfork::fork_named(dir, OsStr::new(name), contents),
        (Some(_), [_, _]) => Err(io::Error::other(
            "--name names the fork next to the directory, and a second argument says where to put it; give one or the other",
        )),
        _ => Err(io::Error::other(
            "too many arguments: a directory, and a fork",
        )),
    }
}
