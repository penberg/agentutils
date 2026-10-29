//! Git, which tools such as `gh` run, and which runs what the repository's
//! configuration and hooks tell it to, both of which the command can write.
//!
//! So a tool runs git with a clone of the repository's `.git` made where the
//! command cannot write it, and only if its configuration differs from what
//! it was when `confine` started in nothing but what `gh` and `git push`
//! write themselves, with hooks off. The clone then takes the place of
//! `.git`, so that what the tool did to it, such as setting the branch it
//! pushed to, is kept.

use std::{
    collections::HashSet,
    env, fs, io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use super::Finish;
use crate::at;

/// The repository of the directory the command is confined to, if it is
/// one.
pub struct Repo {
    /// The directory.
    dir: PathBuf,
    /// Where clones of `.git` are made, which the command may not write to.
    private: PathBuf,
    /// The configuration of `.git` when `confine` started, if it was a
    /// repository.
    config: Option<HashSet<Vec<u8>>>,
    /// How many clones of `.git` have been made, to name the next.
    clones: AtomicU64,
}

impl Repo {
    /// The repository of `dir`, whose `.git` is cloned to `private`.
    pub fn new(dir: &Path, private: &Path) -> io::Result<Repo> {
        let git = dir.join(".git");
        let config = if git.is_dir() {
            Some(config(&git)?)
        } else {
            None
        };
        Ok(Repo {
            dir: dir.to_path_buf(),
            private: private.to_path_buf(),
            config,
            clones: AtomicU64::new(0),
        })
    }

    /// Has git, as `command` runs it, use a clone of `.git`, with hooks and
    /// the file system monitor off, and returns what puts the clone in its
    /// place once `command` has run.
    pub fn isolate(&self, command: &mut Command) -> io::Result<Finish> {
        let clone = self.clone_git()?;
        command
            .env(
                "GIT_DIR",
                clone.clone().unwrap_or_else(|| self.private.join("none")),
            )
            .env("GIT_WORK_TREE", &self.dir);
        // Hooks and the file system monitor are programs in, or named by,
        // `.git`, so are turned off whatever it says. Variables of this kind
        // the user set are kept.
        let n: usize = env::var("GIT_CONFIG_COUNT")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let off = [("core.hooksPath", "/dev/null"), ("core.fsmonitor", "false")];
        for (i, (key, value)) in off.iter().enumerate() {
            command
                .env(format!("GIT_CONFIG_KEY_{}", n + i), key)
                .env(format!("GIT_CONFIG_VALUE_{}", n + i), value);
        }
        command.env("GIT_CONFIG_COUNT", (n + off.len()).to_string());
        let git = self.dir.join(".git");
        Ok(Box::new(move || {
            let Some(clone) = clone else {
                return Ok(());
            };
            restore(&git, &clone).map_err(|e| {
                let _ = fs::remove_dir_all(&clone);
                io::Error::new(
                    e.kind(),
                    format!(".git: what was changed in it is lost: {e}"),
                )
            })
        }))
    }

    /// Clones `.git` to where the command cannot write, and returns where,
    /// if its configuration is safe for git to run with. If the directory
    /// was not a repository, there is none.
    fn clone_git(&self) -> io::Result<Option<PathBuf>> {
        let Some(original) = &self.config else {
            return Ok(None);
        };
        let git = self.dir.join(".git");
        if !git.symlink_metadata().is_ok_and(|meta| meta.is_dir()) {
            return Err(io::Error::other(format!(
                "{}: no longer a directory",
                git.display()
            )));
        }
        let n = self.clones.fetch_add(1, Ordering::Relaxed);
        let clone = self.private.join(format!("{n}.git"));
        clonefile(&git, &clone).map_err(|e| at(&clone, e))?;
        let checked = check(&clone, original);
        if checked.is_err() {
            let _ = fs::remove_dir_all(&clone);
        }
        checked.map(|()| Some(clone))
    }
}

/// Puts `clone` in the place of `git`.
fn restore(git: &Path, clone: &Path) -> io::Result<()> {
    let old = clone.with_extension("old");
    fs::rename(git, &old)?;
    if let Err(e) = fs::rename(clone, git) {
        let _ = fs::rename(&old, git);
        return Err(e);
    }
    fs::remove_dir_all(&old)
}

/// Checks that the configuration of `git` differs from `original` in
/// nothing but what `gh` and `git push` write, and that it does not name
/// another directory to take it from.
fn check(git: &Path, original: &HashSet<Vec<u8>>) -> io::Result<()> {
    if git.join("commondir").symlink_metadata().is_ok() {
        return Err(io::Error::other(
            ".git/commondir: made by the command, so it is not run",
        ));
    }
    for entry in config(git)? {
        if !original.contains(&entry) && !written_by_gh(&entry) {
            let entry = String::from_utf8_lossy(&entry);
            let key = entry.split('\n').next().unwrap_or_default();
            return Err(io::Error::other(format!(
                ".git/config: {key} changed by the command, so it is not run"
            )));
        }
    }
    Ok(())
}

/// The entries of the configuration of `git`, each a key and its value, as
/// `git config --list` gives them, from `config` and `config.worktree`.
fn config(git: &Path) -> io::Result<HashSet<Vec<u8>>> {
    let mut entries = HashSet::new();
    for file in ["config", "config.worktree"] {
        let path = git.join(file);
        if path.symlink_metadata().is_err() {
            continue;
        }
        // Run outside the repository, so that git reads only the file.
        let out = Command::new("git")
            .current_dir("/")
            .args(["config", "--no-includes", "--null", "--list", "--file"])
            .arg(&path)
            .stderr(Stdio::piped())
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "{}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        entries.extend(
            out.stdout
                .split(|&b| b == 0)
                .filter(|entry| !entry.is_empty())
                .map(<[u8]>::to_vec),
        );
    }
    Ok(entries)
}

/// Whether `entry`, a key and its value, is one that `gh` or `git push`
/// write: the branch a branch is pushed to, and the remotes `gh` adds and
/// marks, by URLs that run nothing.
fn written_by_gh(entry: &[u8]) -> bool {
    let entry = String::from_utf8_lossy(entry);
    let (key, value) = entry.split_once('\n').unwrap_or((&entry, ""));
    let (Some((section, _)), Some((_, name))) = (key.split_once('.'), key.rsplit_once('.')) else {
        return false;
    };
    match (section, name) {
        ("branch", "remote" | "merge" | "pushremote" | "gh-merge-base") => true,
        ("remote", "fetch" | "gh-resolved") => true,
        ("remote", "url" | "pushurl") => ["https://", "ssh://", "git@"]
            .iter()
            .any(|scheme| value.starts_with(scheme)),
        _ => false,
    }
}

/// Clones the directory tree at `from` to `to`, sharing its blocks.
fn clonefile(from: &Path, to: &Path) -> io::Result<()> {
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // `CLONE_NOFOLLOW`: symbolic links are cloned as links.
    if unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0x0001) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_only_what_gh_writes() {
        assert!(written_by_gh(b"branch.fix/a.b.remote\norigin"));
        assert!(written_by_gh(
            b"remote.fork.url\nhttps://github.com/a/b.git"
        ));
        assert!(!written_by_gh(b"remote.fork.url\n/tmp/x"));
        assert!(!written_by_gh(b"remote.origin.uploadpack\nsh -c x"));
        assert!(!written_by_gh(b"core.pager\nsh -c x"));
        assert!(!written_by_gh(b"include.path\n/tmp/x"));
    }
}
