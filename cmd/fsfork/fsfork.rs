//! `fsfork` forks a directory: it makes a copy-on-write clone of it, the
//! fork, and another, the base, which keeps what the directory was when it
//! was forked, for `fsdiff` to compare the fork with. With `-l`, it lists
//! the forks of a directory instead, and with `--prune`, removes those done
//! with.

mod forks;

use std::{
    env,
    ffi::OsStr,
    io::{self, IsTerminal, Write},
    os::unix::{ffi::OsStringExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use argh::FromArgs;
use fsfork::{Contents, message};

/// How to give the arguments, as in the man page, a line for each form.
const USAGE: &str =
    "fsfork [--no-ignore] [--print] [-n name] [dir [fork]]\nfsfork -l [dir]\nfsfork --prune [dir]";

/// Fork a directory: make a copy-on-write clone of it, and start a shell there
/// or, if not on a terminal, print where it is.
#[derive(FromArgs)]
struct Args {
    /// list the forks of the directory, with how long each has been left
    /// alone and what changed in it, rather than fork it
    #[argh(switch, short = 'l')]
    list: bool,

    /// remove the forks of the directory that are unchanged or applied, and
    /// the bases left over from forks removed without them, rather than fork
    /// it
    #[argh(switch)]
    prune: bool,

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
    let args: Args = match message::parse(USAGE, 1) {
        Ok(args) => args,
        Err(code) => return code,
    };
    if args.list || args.prune {
        return list_or_prune(args);
    }
    // A shell needs both: with stdout captured, as in `cd $(fsfork)`, the
    // path is wanted, and with stdin not a terminal, a shell would run what
    // it reads from there as commands.
    let shell = !args.print && io::stdin().is_terminal() && io::stdout().is_terminal();
    let fork = match fsfork(args) {
        Ok(fork) => fork,
        Err(Wrong::Usage(problem)) => return message::usage_error(problem, USAGE, 1),
        Err(Wrong::Failed(e)) => {
            message::say(message::describe(&e));
            return ExitCode::FAILURE;
        }
    };
    let done = if shell {
        start_shell(&fork)
    } else {
        print(fork)
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            message::say(message::describe(&e));
            ExitCode::FAILURE
        }
    }
}

/// Lists or prunes the forks of the directory `args` give, as they ask.
fn list_or_prune(args: Args) -> ExitCode {
    let option = if args.list { "-l" } else { "--prune" };
    let conflict = if args.list && args.prune {
        Some("-l cannot be given with --prune".to_string())
    } else if args.print {
        Some(format!("{option} cannot be given with --print"))
    } else if args.no_ignore {
        Some(format!("{option} cannot be given with --no-ignore"))
    } else if args.name.is_some() {
        Some(format!("{option} cannot be given with -n"))
    } else if args.paths.len() > 1 {
        Some("too many arguments".to_string())
    } else {
        None
    };
    if let Some(problem) = conflict {
        return message::usage_error(&problem, USAGE, 1);
    }
    let dir = args.paths.first().map_or(Path::new("."), PathBuf::as_path);
    let done = if args.list {
        forks::list(dir)
    } else {
        forks::prune(dir)
    };
    match done {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            message::say(message::describe(&e));
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
        message::say(message::describe(&e));
    }
    Err(fsfork::at(Path::new(&shell), e))
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

/// Why the directory was not forked.
enum Wrong {
    /// The arguments were not right, for this reason.
    Usage(&'static str),
    /// Forking it failed.
    Failed(io::Error),
}

/// Forks the directory `args` give, and returns where the fork is.
fn fsfork(args: Args) -> Result<PathBuf, Wrong> {
    let contents = if args.no_ignore {
        Contents::Everything
    } else {
        Contents::NotIgnored
    };
    let forked = match (&args.name, &args.paths[..]) {
        (None, []) => fsfork::fork(Path::new("."), contents),
        (None, [dir]) => fsfork::fork(dir, contents),
        (None, [dir, fork]) => fsfork::fork_to(dir, fork, contents),
        (Some(name), []) => fsfork::fork_named(Path::new("."), OsStr::new(name), contents),
        (Some(name), [dir]) => fsfork::fork_named(dir, OsStr::new(name), contents),
        (Some(_), [_, _]) => return Err(Wrong::Usage("-n cannot be given with a fork")),
        _ => return Err(Wrong::Usage("too many arguments")),
    };
    forked.map_err(Wrong::Failed)
}
