//! `confine` runs a command confined to a directory, a fork of the working
//! directory unless it is given one: the command may write there, to
//! temporary directories, and to caches, but nowhere else, and may read in
//! the home directory only what it needs, and so no secrets such as SSH keys.

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
    env,
    ffi::OsStr,
    io,
    os::unix::{
        fs::PermissionsExt,
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{ExitCode, ExitStatus},
};

use argh::FromArgs;
use fsfork::{Contents, at, message};

/// How to give the arguments, as in the man page.
const USAGE: &str = "confine [-w path]... [-r path]... [-n name] [--no-ignore] [--rm | -d dir] [command [argument ...]]";

/// Directories under the home directory that the command may write to, so
/// that builds and package managers keep working.
const CACHES: &[&str] = &[".cargo", ".rustup", ".cache", ".npm", "Library/Caches"];

/// Paths under the home directory that the command may read. It may read
/// nothing else there but what it may write to, so that secrets are kept
/// from it without knowing where each one is.
const READABLE: &[&str] = &[
    // Configuration.
    ".gitconfig",
    ".config/git",
    ".claude.json",
    ".CFUserTextEncoding",
    // Toolchains, and the commands installed with them.
    ".local/bin",
    ".local/lib",
    ".local/share/claude",
    ".nvm",
    ".volta",
    ".bun",
    ".deno",
    ".pyenv",
    ".rbenv",
    // The macOS keychains, whose items the keychain itself guards, and which
    // Claude Code keeps its login in.
    "Library/Keychains",
];

/// The terminal's signals, which reach the command too, and which `confine`
/// outlives, with `--rm`, to remove the fork.
const SIGNALS: [i32; 3] = [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP];

/// Run a command in a fork of the working directory, or in a directory, that
/// may write only there, to temporary directories and caches, and may read
/// in the home directory only what it needs.
#[derive(FromArgs)]
struct Args {
    /// another path the command may write to; can be repeated
    #[argh(option, short = 'w')]
    write: Vec<PathBuf>,

    /// another path the command may read, in the home directory; can be
    /// repeated
    #[argh(option, short = 'r')]
    read: Vec<PathBuf>,

    /// the directory the command runs in and may write to, rather than a
    /// fork of the working directory
    #[argh(option, short = 'd')]
    dir: Option<PathBuf>,

    /// name the fork <dir>.<name> rather than after the first number not
    /// taken
    #[argh(option, short = 'n', arg_name = "name")]
    name: Option<String>,

    /// remove the fork when the command exits
    #[argh(switch)]
    rm: bool,

    /// fork what .gitignore files ignore too, which is otherwise left out
    #[argh(switch)]
    no_ignore: bool,

    /// the command to run and its arguments; the shell if none
    #[argh(positional, greedy)]
    command: Vec<String>,
}

/// What became of the command.
enum Ran {
    /// It ran, and exited with this status.
    Exited(ExitStatus),
    /// It could not be run, for this reason.
    Failed(io::Error),
}

fn main() -> ExitCode {
    let args: Args = match message::parse(USAGE, 1) {
        Ok(args) => args,
        Err(code) => return code,
    };
    if args.dir.is_some() {
        let conflict = if args.rm {
            Some("--rm")
        } else if args.no_ignore {
            Some("--no-ignore")
        } else if args.name.is_some() {
            Some("-n")
        } else {
            None
        };
        if let Some(option) = conflict {
            let problem = format!("{option} cannot be given with -d, which makes no fork");
            return message::usage_error(&problem, USAGE, 1);
        }
    }
    let e = match confine(&args) {
        Ok(Ran::Exited(status)) => return exit_as(status),
        Ok(Ran::Failed(e)) => e,
        Err(e) => {
            message::say(message::describe(&e));
            return ExitCode::FAILURE;
        }
    };
    // As env(1) does: 127 if the command was not found, 126 if it could not
    // be run.
    message::say(message::describe(&e));
    if e.kind() == io::ErrorKind::NotFound {
        ExitCode::from(127)
    } else {
        ExitCode::from(126)
    }
}

/// Runs the command `args` give confined to its directory, or to a fork of
/// the working directory. The fork is removed if the command could not be
/// run, or with `--rm`, after it exits.
fn confine(args: &Args) -> io::Result<Ran> {
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
    let mut readable: Vec<PathBuf> = READABLE
        .iter()
        .map(|path| home.join(path))
        .filter_map(|path| path.canonicalize().ok())
        .collect();
    for path in &args.read {
        readable.push(path.canonicalize().map_err(|e| at(path, e))?);
    }
    let hidden: Vec<PathBuf> = home.canonicalize().into_iter().collect();
    let command = if args.command.is_empty() {
        vec![env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())]
    } else {
        args.command.clone()
    };
    // Before forking, so that a command that is not there makes no fork.
    if let Err(e) = find_in_path(&command[0]) {
        return Ok(Ran::Failed(e));
    }
    if let Some(dir) = &args.dir {
        let typed = dir;
        let dir = dir.canonicalize().map_err(|e| at(dir, e))?;
        if !dir.is_dir() {
            return Err(at(typed, io::Error::from_raw_os_error(libc::ENOTDIR)));
        }
        return run(&command, &dir, &writable, &readable, &hidden, false);
    }
    let contents = if args.no_ignore {
        Contents::Everything
    } else {
        Contents::NotIgnored
    };
    let fork = match &args.name {
        Some(name) => fsfork::fork_named(Path::new("."), OsStr::new(name), contents)?,
        None => fsfork::fork(Path::new("."), contents)?,
    };
    eprintln!("confine: forked to {}", fork.display());
    let ran = run(&command, &fork, &writable, &readable, &hidden, args.rm);
    if (args.rm || !matches!(ran, Ok(Ran::Exited(_))))
        && let Err(e) = fsfork::remove(&fork)
    {
        message::say(message::describe(&e));
    }
    ran
}

/// Runs `command` in `dir`, confined to it: in place of this process, or,
/// to `wait` for it, as a child.
fn run(
    command: &[String],
    dir: &Path,
    writable: &[PathBuf],
    readable: &[PathBuf],
    hidden: &[PathBuf],
    wait: bool,
) -> io::Result<Ran> {
    // So that it is said to be not found by `confine`, rather than by the
    // sandbox, which is what is run.
    if let Err(e) = find_in(&command[0], dir) {
        return Ok(Ran::Failed(e));
    }
    let mut confined = os::confine(command, dir, writable, readable, hidden)?;
    // Claude Code would otherwise ask whether to trust every new directory,
    // and accepting would write to `~/.claude.json`, which it may not.
    confined.current_dir(dir).env("CLAUDE_CODE_SANDBOXED", "1");
    if !wait {
        return Ok(Ran::Failed(at(Path::new(&command[0]), confined.exec())));
    }
    for signal in SIGNALS {
        unsafe { libc::signal(signal, libc::SIG_IGN) };
    }
    // The command gets the terminal's signals as they are by default.
    unsafe {
        confined.pre_exec(|| {
            for signal in SIGNALS {
                libc::signal(signal, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    Ok(match confined.status() {
        Ok(status) => Ran::Exited(status),
        Err(e) => Ran::Failed(at(Path::new(&command[0]), e)),
    })
}

/// Exits as `status` says the command did: with its code, or by the signal
/// that ended it.
fn exit_as(status: ExitStatus) -> ExitCode {
    if let Some(signal) = status.signal() {
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
        return ExitCode::from(128 + signal as u8);
    }
    ExitCode::from(status.code().unwrap_or(1) as u8)
}

/// Finds `command`, if it has no `/` in it, in `$PATH`, as a shell would.
fn find_in_path(command: &str) -> io::Result<()> {
    if command.contains('/') {
        return Ok(());
    }
    let path = env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = env::split_paths(&path).collect();
    // A relative directory is in the one the command runs in, not this one.
    let found = dirs.iter().any(|dir| {
        dir.is_relative()
            || dir
                .join(command)
                .metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    });
    if found {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{command}: command not found"),
        ))
    }
}

/// Finds `command`, if it has a `/` in it, from `dir`, where it runs.
fn find_in(command: &str, dir: &Path) -> io::Result<()> {
    if !command.contains('/') {
        return Ok(());
    }
    dir.join(command)
        .metadata()
        .map(|_| ())
        .map_err(|e| at(Path::new(command), e))
}
