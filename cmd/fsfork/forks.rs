//! The forks of a directory, as `fsfork -l` lists them: with how long each
//! has been left alone and what changed in it, so that those done with can
//! be told from those not.

use std::{
    fmt, fs,
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
    /// `files` changed in it, of which the directory has `applied` now.
    Changed { files: usize, applied: usize },
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
            State::Changed { files, applied } => {
                let s = if *files == 1 { "" } else { "s" };
                write!(f, "{files} file{s} changed")?;
                if *applied > 0 {
                    write!(f, ", {applied} applied")?;
                }
                Ok(())
            }
            State::LeftOver => f.write_str("base left over, with no fork"),
            State::Unknown => f.write_str("cannot be compared with its base"),
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

/// Lists the forks of `dir`, those left alone longest first, and returns
/// whether all could be looked into.
pub fn list(dir: &Path) -> io::Result<bool> {
    let (forks, all) = look(dir)?;
    let now = SystemTime::now();
    let rows: Vec<_> = forks
        .iter()
        .map(|fork| row(fork, now, &fork.state))
        .collect();
    print(&rows)?;
    Ok(all)
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
    let mut applied = 0;
    for path in &changed {
        if repo.applied(&before, &after, Some(path), dir)? {
            applied += 1;
        }
    }
    let files = changed.len();
    Ok((State::Changed { files, applied }, touched))
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

/// Prints `rows` on stdout, in columns as wide as the widest of each.
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
                line.push_str(&format!("{column:width$}  "));
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
