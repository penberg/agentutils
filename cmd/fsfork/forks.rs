//! The forks of a directory, as `fsfork -l` lists them: with how long each
//! has been left alone and what changed in it, so that those done with can
//! be told from those not; and as `fsfork --prune` removes those done with.

use std::{
    env, fmt, fs,
    io::{self, Write},
    path::Path,
    time::{Duration, SystemTime},
};

use fsfork::{Found, git, message};

/// A fork, looked into.
struct Looked {
    found: Found,
    state: State,
    /// When it was last changed, or made, if it was not.
    touched: SystemTime,
}

/// What is in a fork.
enum State {
    /// Nothing changed in it.
    Unchanged,
    /// Something did, and the directory has it all now.
    Applied,
    /// Something did, and the directory does not have the changes to
    /// `files` of the files.
    NotApplied { files: usize },
    /// The fork is gone, and its base is not.
    LeftOver,
    /// It could not be compared with its base.
    Unknown,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            State::Unchanged => f.write_str("unchanged"),
            State::Applied => f.write_str("applied"),
            State::NotApplied { files: 1 } => f.write_str("1 file not applied"),
            State::NotApplied { files } => write!(f, "{files} files not applied"),
            State::LeftOver => f.write_str("base only"),
            State::Unknown => f.write_str("unknown"),
        }
    }
}

impl Looked {
    /// Where it is: the fork, or if only its base is left, the base.
    fn path(&self) -> &Path {
        match self.state {
            State::LeftOver => &self.found.base,
            _ => &self.found.fork,
        }
    }
}

/// Lists the forks of `dir`, under a header, those left alone longest first,
/// and returns whether all could be looked into.
pub fn list(dir: &Path) -> io::Result<bool> {
    let (forks, all) = look(dir)?;
    let now = SystemTime::now();
    let header = ["FORK", "UPDATED", "STATUS"].map(String::from);
    let rows: Vec<_> = std::iter::once(header)
        .chain(forks.iter().map(|fork| row(fork, now, &fork.state)))
        .collect();
    print(&rows)?;
    Ok(all)
}

/// How long a fork must be left alone for `prune` to remove it: one just
/// made, with nothing changed in it yet, may have a command starting there,
/// and one just applied may have more to come.
const LEFT_ALONE: Duration = Duration::from_secs(60 * 60);

/// How long a fork must be left alone for `prune` to remove it: `LEFT_ALONE`,
/// or in a debug build, for tests, which cannot make a fork older, as many
/// seconds as `FSFORK_TEST_LEFT_ALONE` says.
fn left_alone() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(secs) = env::var("FSFORK_TEST_LEFT_ALONE")
        .ok()
        .and_then(|secs| secs.parse().ok())
    {
        return Duration::from_secs(secs);
    }
    LEFT_ALONE
}

/// Removes the forks of `dir` that are unchanged or applied and were left
/// alone long enough, and the bases left over from forks removed without
/// them, saying on stdout each it removed, and on stderr how many it kept,
/// and why; and returns whether all could be looked into and removed.
pub fn prune(dir: &Path) -> io::Result<bool> {
    let (forks, mut all) = look(dir)?;
    let now = SystemTime::now();
    let cwd = env::current_dir().and_then(|cwd| cwd.canonicalize()).ok();
    let long_enough = left_alone();
    let mut kept = [0; Kept::ALL.len()];
    let mut out = io::stdout().lock();
    for fork in &forks {
        let why = match fork.state {
            State::NotApplied { .. } => Some(Kept::NotApplied),
            State::Unknown => Some(Kept::NotCompared),
            _ if now.duration_since(fork.touched).unwrap_or_default() < long_enough => {
                Some(Kept::Recent)
            }
            _ if cwd.as_ref().is_some_and(|cwd| cwd.starts_with(fork.path())) => Some(Kept::InUse),
            _ => match remove(fork) {
                Ok(()) => {
                    let path = message::shown(fork.path());
                    writeln!(out, "removed {}", path.display())?;
                    None
                }
                Err(e) => {
                    message::say(message::describe(&e));
                    all = false;
                    Some(Kept::NotRemoved)
                }
            },
        };
        if let Some(why) = why {
            kept[why as usize] += 1;
        }
    }
    out.flush()?;
    let kept: Vec<String> = Kept::ALL
        .iter()
        .zip(kept)
        .filter(|(_, n)| *n > 0)
        .map(|(why, n)| format!("{n} {}", why.word()))
        .collect();
    if !kept.is_empty() {
        message::say(format!("kept {}", kept.join(", ")));
    }
    Ok(all)
}

/// Why `prune` kept a fork.
#[derive(Clone, Copy)]
enum Kept {
    /// It has changes the directory does not.
    NotApplied,
    /// It was left alone too little.
    Recent,
    /// The working directory is in it.
    InUse,
    /// It could not be compared with its base.
    NotCompared,
    /// Removing it failed.
    NotRemoved,
}

impl Kept {
    /// Every reason, in the order they are said.
    const ALL: [Kept; 5] = [
        Kept::NotApplied,
        Kept::Recent,
        Kept::InUse,
        Kept::NotCompared,
        Kept::NotRemoved,
    ];

    /// What is said of those kept for this reason, as in "2 not applied".
    fn word(self) -> &'static str {
        match self {
            Kept::NotApplied => "not applied",
            Kept::Recent => "recent",
            Kept::InUse => "in use",
            Kept::NotCompared => "not compared",
            Kept::NotRemoved => "not removed",
        }
    }
}

/// Removes `fork`, and its base.
fn remove(fork: &Looked) -> io::Result<()> {
    match fork.state {
        State::LeftOver => {
            fs::remove_dir_all(&fork.found.base).map_err(|e| fsfork::at(&fork.found.base, e))
        }
        _ => fsfork::remove(&fork.found.fork),
    }
}

/// Looks into the forks of `dir`, and returns them, those left alone longest
/// first, and whether all could be looked into, having said why not.
fn look(dir: &Path) -> io::Result<(Vec<Looked>, bool)> {
    let found = fsfork::forks(dir)?;
    if found.is_empty() {
        return Ok((Vec::new(), true));
    }
    let dir = dir.canonicalize()?;
    // The terminal's signals reach git too, which exits; `fsfork` outlives
    // it to remove its repository.
    for signal in git::SIGNALS {
        unsafe { libc::signal(signal, libc::SIG_IGN) };
    }
    let repo = git::Repo::new()?;
    let mut all = true;
    let mut forks: Vec<Looked> = found
        .into_iter()
        .map(|found| match state(&repo, &dir, &found) {
            Ok((state, touched)) => Looked {
                found,
                state,
                touched,
            },
            Err(e) => {
                message::say(message::describe(&e));
                all = false;
                let touched = found.made;
                Looked {
                    found,
                    state: State::Unknown,
                    touched,
                }
            }
        })
        .collect();
    // Those left alone longest, the likeliest to be done with, first.
    forks.sort_by_key(|fork| fork.touched);
    Ok((forks, all))
}

/// What is in `fork`, of `dir`, and when it was last changed, or made, if it
/// was not.
fn state(repo: &git::Repo, dir: &Path, fork: &Found) -> io::Result<(State, SystemTime)> {
    if fork.left_over {
        return Ok((State::LeftOver, fork.made));
    }
    let (before, after) = repo.record(&fork.base, &fork.fork)?;
    let changed = repo.changed(&before, &after)?;
    // A new directory at the top, such as for a build, changes the fork's
    // own time.
    let made = modified(&fork.fork, Path::new("")).map_or(fork.made, |t| t.max(fork.made));
    if changed.is_empty() {
        return Ok((State::Unchanged, made));
    }
    let touched = changed
        .iter()
        .filter_map(|path| modified(&fork.fork, path))
        .fold(made, SystemTime::max);
    if repo.applied(&before, &after, None, dir)? {
        return Ok((State::Applied, touched));
    }
    let mut files = 0;
    for path in &changed {
        if !repo.applied(&before, &after, Some(path), dir)? {
            files += 1;
        }
    }
    // Each file's changes may be there without the whole patch undoing.
    let state = match files {
        0 => State::Applied,
        files => State::NotApplied { files },
    };
    Ok((state, touched))
}

/// When `path` in `fork` was last changed, or if it is gone, the directory
/// it was in, which changed when it went.
fn modified(fork: &Path, path: &Path) -> Option<SystemTime> {
    path.ancestors()
        .map(|path| fork.join(path))
        .find_map(|path| fs::symlink_metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
}

/// The columns of `fork`'s line, as of `now`, saying `what` of it.
fn row(fork: &Looked, now: SystemTime, what: &dyn fmt::Display) -> [String; 3] {
    [
        message::shown(fork.path()).display().to_string(),
        age(now.duration_since(fork.touched).unwrap_or_default()),
        what.to_string(),
    ]
}

/// Prints `rows` on stdout, in columns as wide as the widest of each, and
/// three spaces apart.
fn print<const N: usize>(rows: &[[String; N]]) -> io::Result<()> {
    let mut widths = [0; N];
    for row in rows {
        for (width, column) in widths.iter_mut().zip(row) {
            *width = (*width).max(column.chars().count());
        }
    }
    let mut out = io::stdout().lock();
    for row in rows {
        let mut line = String::new();
        for (i, (column, width)) in row.iter().zip(widths).enumerate() {
            if i + 1 == N {
                line.push_str(column);
            } else {
                line.push_str(&format!("{column:width$}   "));
            }
        }
        writeln!(out, "{line}")?;
    }
    out.flush()
}

/// How long ago something was, `ago` since, roughly, as in "3 days ago".
fn age(ago: Duration) -> String {
    let secs = ago.as_secs();
    let (n, unit) = match secs {
        0..60 => return "just now".to_string(),
        60..3600 => (secs / 60, "minute"),
        3600..86400 => (secs / 3600, "hour"),
        86400..1209600 => (secs / 86400, "day"),
        1209600..5184000 => (secs / 604800, "week"),
        5184000..63072000 => (secs / 2592000, "month"),
        _ => (secs / 31536000, "year"),
    };
    let s = if n == 1 { "" } else { "s" };
    format!("{n} {unit}{s} ago")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_long_ago_roughly() {
        let ago = |secs| age(Duration::from_secs(secs));
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(3599), "59 minutes ago");
        assert_eq!(ago(7200), "2 hours ago");
        assert_eq!(ago(86400 * 3), "3 days ago");
        assert_eq!(ago(86400 * 14), "2 weeks ago");
        assert_eq!(ago(86400 * 90), "3 months ago");
        assert_eq!(ago(86400 * 800), "2 years ago");
    }
}
