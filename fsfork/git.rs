//! Git, as `fsdiff` and `fsfork -l` use it: to record two directories as
//! trees, and to show the difference between them.
//!
//! Git never uses a repository in either directory, whose configuration could
//! make it run any command, as the command that changed the fork could have
//! changed it. The trees are recorded in a repository of the command's own,
//! made in the temporary directory and removed after, by git with no
//! configuration but its defaults.

use std::{
    cell::RefCell,
    env,
    ffi::OsStr,
    fs, io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{self, Command, ExitStatus},
};

use crate::message::{self, shown};

/// The signals from the terminal, which the caller ignores and git does not.
pub const SIGNALS: [i32; 3] = [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP];

/// The variables that would point git at another repository than the one it
/// is given.
const REPOSITORY: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
];

/// A repository of the command's own, removed when dropped.
pub struct Repo {
    dir: PathBuf,
    /// The directory last recorded as `old`, whose files the index of `old`
    /// says how they were when hashed.
    indexed: RefCell<Option<PathBuf>>,
}

impl Repo {
    /// A new, empty repository in the temporary directory.
    pub fn new() -> io::Result<Repo> {
        let tmp = env::temp_dir();
        let mut n = 0;
        let dir = loop {
            let dir = tmp.join(format!("{}.{}.{n}", message::prog(), process::id()));
            match fs::create_dir(&dir) {
                Ok(()) => break dir,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        };
        let repo = Repo {
            dir,
            indexed: RefCell::new(None),
        };
        let made = status(bare().args(["init", "--quiet", "--bare"]).arg(&repo.dir))?;
        if !made.success() {
            return Err(io::Error::other("git could not make a repository"));
        }
        Ok(repo)
    }

    /// Records `old` and `new` as trees, and returns their IDs.
    ///
    /// Both leave out what git ignores, by the `.gitignore` files in them,
    /// but `new` only what was not in `old`: its index starts as `old`'s, and
    /// git does not ignore a file in the index.
    pub fn record(&self, old: &Path, new: &Path) -> io::Result<(String, String)> {
        let old_index = self.dir.join("old.index");
        let new_index = self.dir.join("new.index");
        let last = self.indexed.borrow_mut().take();
        let before = self.snapshot(old, &old_index, last.as_deref())?;
        *self.indexed.borrow_mut() = Some(old.to_path_buf());
        fs::copy(&old_index, &new_index)?;
        let after = self.snapshot(new, &new_index, Some(old))?;
        Ok((before, after))
    }

    /// Shows the difference between the trees `before` and `after` as a patch
    /// that `git apply` applies, or with `stat`, which files changed and how
    /// much, and returns whether there is one.
    ///
    /// Git uses the user's configuration here, for colours and the pager, but
    /// not to change what the patch says.
    pub fn diff(&self, before: &str, after: &str, stat: bool) -> io::Result<bool> {
        let mut git = Command::new("git");
        for var in REPOSITORY {
            git.env_remove(var);
        }
        git.arg("--git-dir").arg(&self.dir).args([
            "diff",
            "--exit-code",
            "--color=auto",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            if stat { "--stat" } else { "--binary" },
            before,
            after,
        ]);
        match status(&mut git)?.code() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(io::Error::other("git could not compare the trees")),
        }
    }

    /// The paths that differ between the trees `before` and `after`, as in
    /// `diff` but by git with no configuration but its defaults, a rename
    /// being both of its paths.
    pub fn changed(&self, before: &str, after: &str) -> io::Result<Vec<PathBuf>> {
        let listed = self
            .git()
            .args(["diff", "--name-only", "--no-renames", "-z", before, after])
            .stderr(process::Stdio::inherit())
            .output()?;
        if !listed.status.success() {
            return Err(io::Error::other("git could not compare the trees"));
        }
        Ok(listed
            .stdout
            .split(|&b| b == 0)
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(OsStr::from_bytes(path)))
            .collect())
    }

    /// How many of the files changed between the trees `before` and `after`
    /// have changes that `dir` does not: that could not be undone in `dir`,
    /// as they could if they were applied to it, even if it changed after.
    ///
    /// Git checks every file's changes in one run, saying which file it is
    /// checking before any error about it, so that the files are told apart
    /// without reading their paths back from what it says.
    pub fn unapplied(&self, before: &str, after: &str, dir: &Path) -> io::Result<usize> {
        let patch = self
            .git()
            .args(["diff", "--binary", "--no-renames", before, after])
            .stderr(process::Stdio::inherit())
            .output()?;
        if !patch.status.success() {
            return Err(io::Error::other("git could not compare the trees"));
        }
        let file = self.dir.join("patch");
        fs::write(&file, &patch.stdout)?;
        let checked = output(
            self.git()
                .arg("--work-tree")
                .arg(dir)
                .args(["apply", "--reverse", "--check", "--verbose"])
                .arg(&file)
                .current_dir(dir)
                .env("LC_ALL", "C")
                .env_remove("LANGUAGE")
                .stdin(process::Stdio::null())
                .stdout(process::Stdio::null()),
        )?;
        if checked.status.success() {
            return Ok(0);
        }
        let mut files = 0;
        let mut checking = false;
        let mut counted = false;
        for line in checked.stderr.split(|&b| b == b'\n') {
            if line.starts_with(b"Checking patch ") {
                checking = true;
                counted = false;
            } else if line.starts_with(b"error: ") && checking && !counted {
                files += 1;
                counted = true;
            }
        }
        match (checked.status.code(), files) {
            (Some(1), 1..) => Ok(files),
            _ => Err(io::Error::other(format!(
                "{}: git could not check the changes in it",
                shown(dir).display()
            ))),
        }
    }

    /// Records the files in `tree` that git does not ignore as a tree,
    /// keeping the index in `index`, which says how the files in `from` were
    /// when hashed, if it is not new, and returns the tree's ID.
    ///
    /// A file is hashed again only if its size or modification time differs
    /// from the index's. Its inode and change time are not compared: a fork's
    /// files are clones of its base's, with new ones but the same times, and
    /// comparing them would hash every file in every fork and base again.
    /// Git compares only the whole seconds of the time then, so a file whose
    /// time differs from `from`'s only within the second is hashed again
    /// first, comparing all the file's stat, staying in the index so that
    /// git does not ignore it.
    fn snapshot(&self, tree: &Path, index: &Path, from: Option<&Path>) -> io::Result<String> {
        if let Some(from) = from {
            self.rehash_changed_within_second(tree, index, from)?;
        }
        let added = status(
            self.git()
                .args([
                    "-c",
                    "core.checkStat=minimal",
                    "-c",
                    "core.trustCtime=false",
                ])
                .arg("--work-tree")
                .arg(tree)
                .args(["add", "--all", "--", "."])
                .current_dir(tree)
                .env("GIT_INDEX_FILE", index),
        )?;
        if !added.success() {
            return Err(io::Error::other(format!(
                "{}: git could not read it",
                shown(tree).display()
            )));
        }
        let written = self
            .git()
            .arg("write-tree")
            .env("GIT_INDEX_FILE", index)
            .output()?;
        if !written.status.success() {
            return Err(io::Error::other(format!(
                "{}: git could not record it",
                shown(tree).display()
            )));
        }
        Ok(String::from_utf8_lossy(&written.stdout).trim().to_string())
    }

    /// Hashes again into `index` the files in `tree` whose size and whole
    /// seconds of modification time are those of the same file in `from`, but
    /// whose time is not.
    fn rehash_changed_within_second(
        &self,
        tree: &Path,
        index: &Path,
        from: &Path,
    ) -> io::Result<()> {
        let listed = self
            .git()
            .args(["ls-files", "-z"])
            .env("GIT_INDEX_FILE", index)
            .stderr(process::Stdio::inherit())
            .output()?;
        if !listed.status.success() {
            return Err(io::Error::other("git could not list its index"));
        }
        let mut changed = Vec::new();
        for path in listed.stdout.split(|&b| b == 0).filter(|p| !p.is_empty()) {
            let path = Path::new(OsStr::from_bytes(path));
            let (Ok(was), Ok(is)) = (
                fs::symlink_metadata(from.join(path)),
                fs::symlink_metadata(tree.join(path)),
            ) else {
                continue;
            };
            if was.file_type() == is.file_type()
                && !is.is_dir()
                && was.size() == is.size()
                && was.mtime() == is.mtime()
                && was.mtime_nsec() != is.mtime_nsec()
            {
                changed.extend_from_slice(path.as_os_str().as_bytes());
                changed.push(0);
            }
        }
        if changed.is_empty() {
            return Ok(());
        }
        let file = self.dir.join("rehash");
        fs::write(&file, &changed)?;
        let rehashed = status(
            self.git()
                .arg("--work-tree")
                .arg(tree)
                .args(["update-index", "-z", "--stdin"])
                .current_dir(tree)
                .env("GIT_INDEX_FILE", index)
                .stdin(fs::File::open(&file)?)
                .stdout(process::Stdio::null()),
        )?;
        if !rehashed.success() {
            return Err(io::Error::other(format!(
                "{}: git could not read it",
                shown(tree).display()
            )));
        }
        Ok(())
    }

    /// A command that runs git on the repository, as `bare` does.
    fn git(&self) -> Command {
        let mut git = bare();
        git.arg("--git-dir").arg(&self.dir);
        git
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A command that runs git with no configuration but its defaults, and none
/// of the variables that change what it does.
fn bare() -> Command {
    let mut git = Command::new("git");
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            git.env_remove(key);
        }
    }
    git.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "advice.addEmbeddedRepo=false"]);
    git
}

/// Runs `command`, with the terminal's signals, which the caller ignores, as
/// they are by default, and waits for it to exit.
fn status(command: &mut Command) -> io::Result<ExitStatus> {
    with_signals(command).status()
}

/// Runs `command` as `status` does, and returns what it printed too.
fn output(command: &mut Command) -> io::Result<process::Output> {
    with_signals(command).output()
}

/// `command`, with the terminal's signals restored to their defaults in it.
fn with_signals(command: &mut Command) -> &mut Command {
    unsafe {
        command.pre_exec(|| {
            for signal in SIGNALS {
                libc::signal(signal, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    command
}
