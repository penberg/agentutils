//! `fsdiff` shows what changed in a fork since it was made, as a patch: the
//! difference between the fork and its base, which `fsfork` made next to it.

use std::{
    io,
    path::{Path, PathBuf},
    process::ExitCode,
};

use argh::FromArgs;
use fsfork::{at, git, message};

/// How to give the arguments, as in the man page, a line for each form.
const USAGE: &str = "fsdiff [--stat] [fork]\nfsdiff [--stat] old new";

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
    let args: Args = match message::parse(USAGE, TROUBLE) {
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
        Err(Wrong::Usage(problem)) => message::usage_error(problem, USAGE, TROUBLE),
        Err(Wrong::Failed(e)) => {
            message::say(message::describe(&e));
            ExitCode::from(TROUBLE)
        }
    }
}

/// Why there is no difference to show.
enum Wrong {
    /// The arguments were not right, for this reason.
    Usage(&'static str),
    /// Showing it failed.
    Failed(io::Error),
}

impl From<io::Error> for Wrong {
    fn from(e: io::Error) -> Self {
        Wrong::Failed(e)
    }
}

/// Shows the difference `args` ask for, and returns whether there is one.
fn fsdiff(args: &Args) -> Result<bool, Wrong> {
    let (old, new) = match &args.paths[..] {
        [old, new] => (directory(old)?, directory(new)?),
        [] | [_] => {
            let fork = args.paths.first().map_or(Path::new("."), PathBuf::as_path);
            let typed = fork;
            let fork = directory(fork)?;
            let base = fsfork::base_of(&fork);
            if !base.is_dir() {
                let name = base.file_name().unwrap_or_default().to_string_lossy();
                return Err(Wrong::Failed(io::Error::other(format!(
                    "{}: not a fork: no {name} next to it",
                    typed.display()
                ))));
            }
            (base, fork)
        }
        _ => return Err(Wrong::Usage("too many arguments")),
    };
    let repo = git::Repo::new()?;
    let (before, after) = repo.record(&old, &new)?;
    Ok(repo.diff(&before, &after, args.stat)?)
}

/// `path`, canonical, if it is a directory.
fn directory(path: &Path) -> io::Result<PathBuf> {
    let dir = path.canonicalize().map_err(|e| at(path, e))?;
    if !dir.is_dir() {
        return Err(at(path, io::Error::from_raw_os_error(libc::ENOTDIR)));
    }
    Ok(dir)
}
