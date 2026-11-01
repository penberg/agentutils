//! What `.gitignore` files ignore in a directory, as git says.
//!
//! Git never uses the directory's repository, whose configuration could make
//! it run any command, as a command confined to a fork could have changed it,
//! and that fork may be what is forked next. It runs on a repository of its
//! own, made in the temporary directory and removed after, with no
//! configuration but its defaults, and reads only the `.gitignore` files and
//! the index, to keep the files in it, which git does not ignore.

use std::{
    collections::HashSet,
    env,
    ffi::OsStr,
    fs, io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{self, Command},
};

/// The paths in a directory that are ignored, and the directories that have
/// some in them.
pub struct Ignored {
    pub paths: HashSet<PathBuf>,
    pub around: HashSet<PathBuf>,
}

/// What is ignored in `dir`, which is canonical.
pub fn ignored(dir: &Path) -> io::Result<Ignored> {
    let repo = Repo::new()?;
    let (mut root, mut index) = repository(dir);
    // A directory that is ignored itself has only its own `.gitignore` files
    // to go by, as all of it would be ignored otherwise.
    if root != dir && repo.ignores(root, dir)? {
        (root, index) = (dir, None);
    }
    let mut ls_files = repo.git(root);
    ls_files
        .args(["ls-files", "-z", "--others", "--ignored", "--directory"])
        .arg("--exclude-per-directory=.gitignore")
        .current_dir(dir)
        .env(
            "GIT_INDEX_FILE",
            index.unwrap_or_else(|| repo.dir.join("index")),
        );
    let listed = run(&mut ls_files)?;
    let mut ignored = Ignored {
        paths: HashSet::new(),
        around: HashSet::new(),
    };
    for path in listed.split(|&b| b == 0).filter(|path| !path.is_empty()) {
        let path = dir.join(OsStr::from_bytes(path));
        for parent in path.ancestors().skip(1) {
            if !parent.starts_with(dir) || !ignored.around.insert(parent.to_path_buf()) {
                break;
            }
        }
        ignored.paths.insert(path);
    }
    Ok(ignored)
}

/// The top of the repository `dir` is in, or `dir` if it is in none, and
/// where the repository's index is.
fn repository(dir: &Path) -> (&Path, Option<PathBuf>) {
    for root in dir.ancestors() {
        let git = root.join(".git");
        if git.is_dir() {
            return (root, Some(git.join("index")));
        }
        // A linked worktree or a submodule, whose `.git` names where it is.
        if let Some(gitdir) = fs::read(&git).ok().and_then(|file| linked(&file)) {
            return (root, Some(root.join(gitdir).join("index")));
        }
    }
    (dir, None)
}

/// The directory that a `.git` file holding `file` names.
fn linked(file: &[u8]) -> Option<PathBuf> {
    let gitdir = file.strip_prefix(b"gitdir: ")?.trim_ascii_end();
    Some(PathBuf::from(OsStr::from_bytes(gitdir)))
}

/// A repository of `fsfork`'s own, removed when dropped.
struct Repo {
    dir: PathBuf,
}

impl Repo {
    /// A new, empty repository in the temporary directory.
    fn new() -> io::Result<Repo> {
        let tmp = env::temp_dir();
        let mut n = 0;
        let dir = loop {
            let dir = tmp.join(format!("fsfork.{}.{n}", process::id()));
            match fs::create_dir(&dir) {
                Ok(()) => break dir,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        };
        let repo = Repo { dir };
        run(git().args(["init", "--quiet", "--bare"]).arg(&repo.dir))?;
        Ok(repo)
    }

    /// A command that runs git on the repository, with `root` for its work
    /// tree.
    fn git(&self, root: &Path) -> Command {
        let mut git = git();
        git.arg("--git-dir")
            .arg(&self.dir)
            .arg("--work-tree")
            .arg(root);
        git
    }

    /// Whether the `.gitignore` files in `root` ignore `dir`, which is in it.
    fn ignores(&self, root: &Path, dir: &Path) -> io::Result<bool> {
        let checked = self
            .git(root)
            .args(["check-ignore", "--quiet", "--no-index", "--"])
            .arg(dir)
            .current_dir(root)
            .output()
            .map_err(|e| io::Error::other(format!("cannot run git: {e}")))?;
        match checked.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(io::Error::other(format!(
                "git: {}",
                String::from_utf8_lossy(&checked.stderr).trim()
            ))),
        }
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A command that runs git with no configuration but its defaults, and none
/// of the variables that change what it does.
fn git() -> Command {
    let mut git = Command::new("git");
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            git.env_remove(key);
        }
    }
    git.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    git
}

/// Runs `command`, and returns what it printed.
fn run(command: &mut Command) -> io::Result<Vec<u8>> {
    let out = command
        .output()
        .map_err(|e| io::Error::other(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_git_file_names_the_directory() {
        assert_eq!(
            linked(b"gitdir: ../.git/worktrees/a\n"),
            Some(PathBuf::from("../.git/worktrees/a"))
        );
        assert_eq!(linked(b"something else"), None);
    }
}
