//! Git, as `fsdiff` uses it: to record two directories as trees, and to show
//! the difference between them.
//!
//! Git never uses a repository in either directory, whose configuration could
//! make it run any command, as the command that changed the fork could have
//! changed it. The trees are recorded in a repository of `fsdiff`'s own, made
//! in the temporary directory and removed after, by git with no configuration
//! but its defaults.

use std::{
    env, fs, io,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{self, Command, ExitStatus},
};

/// The signals from the terminal, which `fsdiff` ignores and git does not.
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

/// A repository of `fsdiff`'s own, removed when dropped.
pub struct Repo {
    dir: PathBuf,
}

impl Repo {
    /// A new, empty repository in the temporary directory.
    pub fn new() -> io::Result<Repo> {
        let tmp = env::temp_dir();
        let mut n = 0;
        let dir = loop {
            let dir = tmp.join(format!("fsdiff.{}.{n}", process::id()));
            match fs::create_dir(&dir) {
                Ok(()) => break dir,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        };
        let repo = Repo { dir };
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
        let before = self.snapshot(old, &old_index)?;
        fs::copy(&old_index, &new_index)?;
        let after = self.snapshot(new, &new_index)?;
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

    /// Records the files in `tree` that git does not ignore as a tree,
    /// keeping the index in `index`, and returns the tree's ID.
    fn snapshot(&self, tree: &Path, index: &Path) -> io::Result<String> {
        let added = status(
            self.git()
                .arg("--work-tree")
                .arg(tree)
                .args(["add", "--all", "--", "."])
                .current_dir(tree)
                .env("GIT_INDEX_FILE", index),
        )?;
        if !added.success() {
            return Err(io::Error::other(format!(
                "git could not read {}",
                tree.display()
            )));
        }
        let written = self
            .git()
            .arg("write-tree")
            .env("GIT_INDEX_FILE", index)
            .output()?;
        if !written.status.success() {
            return Err(io::Error::other(format!(
                "git could not record {}",
                tree.display()
            )));
        }
        Ok(String::from_utf8_lossy(&written.stdout).trim().to_string())
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

/// Runs `command`, with the terminal's signals, which `fsdiff` ignores, as
/// they are by default, and waits for it to exit.
fn status(command: &mut Command) -> io::Result<ExitStatus> {
    unsafe {
        command.pre_exec(|| {
            for signal in SIGNALS {
                libc::signal(signal, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    command.status()
}
