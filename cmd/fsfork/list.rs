//! `fsfork -l`: lists the forks of a directory, with how long each has been
//! left alone and what changed in it, so that those done with can be told
//! from those not.

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use fsfork::{Found, git, message};

/// Lists the forks of `dir`, those left alone longest first, and returns
/// whether all could be looked into.
pub fn list(dir: &Path) -> io::Result<bool> {
    let found = fsfork::forks(dir)?;
    if found.is_empty() {
        return Ok(true);
    }
    let dir = dir.canonicalize()?;
    // The terminal's signals reach git too, which exits; `fsfork` outlives
    // it to remove its repository.
    for signal in git::SIGNALS {
        unsafe { libc::signal(signal, libc::SIG_IGN) };
    }
    let repo = git::Repo::new()?;
    let now = SystemTime::now();
    let mut all = true;
    let mut rows = Vec::new();
    for fork in &found {
        let (state, touched) = match state(&repo, &dir, fork) {
            Ok(state) => state,
            Err(e) => {
                message::say(message::describe(&e));
                all = false;
                ("cannot be compared with its base".to_string(), fork.made)
            }
        };
        let path = if fork.left_over {
            &fork.base
        } else {
            &fork.fork
        };
        rows.push((
            touched,
            [
                message::shown(path).display().to_string(),
                age(now.duration_since(touched).unwrap_or_default()),
                state,
            ],
        ));
    }
    // Those left alone longest, the likeliest to be done with, first.
    rows.sort_by_key(|(touched, _)| *touched);
    let rows: Vec<_> = rows.into_iter().map(|(_, row)| row).collect();
    let path_width = rows.iter().map(|row| row[0].len()).max().unwrap_or(0);
    let age_width = rows.iter().map(|row| row[1].len()).max().unwrap_or(0);
    let mut out = io::stdout().lock();
    for [path, age, state] in rows {
        writeln!(out, "{path:path_width$}  {age:age_width$}  {state}")?;
    }
    out.flush()?;
    Ok(all)
}

/// What is in `fork`, of `dir`: whether anything changed in it since it was
/// made, and if so, whether `dir` has those changes now; and when it was last
/// changed, or made, if it was not.
fn state(repo: &git::Repo, dir: &Path, fork: &Found) -> io::Result<(String, SystemTime)> {
    if fork.left_over {
        return Ok(("base left over, with no fork".to_string(), fork.made));
    }
    let (before, after) = repo.record(&fork.base, &fork.fork)?;
    let changed = repo.changed(&before, &after)?;
    if changed.is_empty() {
        return Ok(("unchanged".to_string(), fork.made));
    }
    let touched = changed
        .iter()
        .filter_map(|path| modified(&fork.fork, path))
        .fold(fork.made, SystemTime::max);
    let applied = changed
        .iter()
        .filter(|path| same(&fork.fork.join(path), &dir.join(path)))
        .count();
    let files = match changed.len() {
        1 => "1 file changed".to_string(),
        n => format!("{n} files changed"),
    };
    let state = match applied {
        0 => files,
        n if n == changed.len() => "applied".to_string(),
        n => format!("{files}, {n} applied"),
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

/// Whether `a` and `b` are the same as git sees them: both missing, links to
/// the same place, or files with the same contents, both executable or
/// neither.
fn same(a: &Path, b: &Path) -> bool {
    let (a_meta, b_meta) = (fs::symlink_metadata(a), fs::symlink_metadata(b));
    let (a_meta, b_meta) = match (a_meta, b_meta) {
        (Err(a), Err(b)) => {
            return a.kind() == io::ErrorKind::NotFound && b.kind() == io::ErrorKind::NotFound;
        }
        (Ok(a), Ok(b)) => (a, b),
        _ => return false,
    };
    let (a_type, b_type) = (a_meta.file_type(), b_meta.file_type());
    if a_type.is_symlink() && b_type.is_symlink() {
        return target(a).is_some_and(|a| target(b).as_ref() == Some(&a));
    }
    if !(a_type.is_file() && b_type.is_file()) {
        return false;
    }
    let executable = |mode: u32| mode & 0o111 != 0;
    executable(a_meta.permissions().mode()) == executable(b_meta.permissions().mode())
        && a_meta.len() == b_meta.len()
        && fs::read(a).is_ok_and(|a| fs::read(b).is_ok_and(|b| a == b))
}

fn target(link: &Path) -> Option<PathBuf> {
    fs::read_link(link).ok()
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
