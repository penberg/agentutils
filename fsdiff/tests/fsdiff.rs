//! Conformance tests: `fsdiff` run as a user runs it, checked against what
//! `fsdiff.1` says it does.
//!
//! Forks are made as `fsfork` makes them, with `cp`, so that the tests need
//! only `fsdiff`.

use std::{
    env, fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
};

#[test]
fn shows_what_changed_in_the_fork_as_a_patch() {
    let world = World::new();
    let dir = world.project();
    let fork = world.fork(&dir);
    write(&fork, "a", "two\n");
    write(&fork, "b", "new\n");
    for (cwd, args) in [(&fork, vec![]), (&world.dir, vec!["project.1"])] {
        let out = world.fsdiff(cwd, &args);
        assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
        let patch = stdout(&out);
        for line in [
            "+++ b/a",
            "-one",
            "+two",
            "new file mode",
            "+++ b/b",
            "+new",
        ] {
            assert!(
                patch.lines().any(|l| l.starts_with(line)),
                "{line}: {patch}"
            );
        }
    }
}

#[test]
fn stat_shows_only_which_files_changed_and_how_much() {
    let world = World::new();
    let dir = world.project();
    let fork = world.fork(&dir);
    write(&fork, "a", "two\n");
    write(&fork, "b", "new\n");
    let stat = stdout(&world.fsdiff(&fork, &["--stat"]));
    assert!(stat.contains(" a | 2 +-"), "{stat}");
    assert!(stat.contains(" b | 1 +"), "{stat}");
    assert!(stat.contains("2 files changed"), "{stat}");
    assert!(!stat.contains("+two"), "{stat}");
}

#[test]
fn exits_with_0_and_shows_nothing_if_nothing_changed() {
    let world = World::new();
    let dir = world.project();
    let fork = world.fork(&dir);
    let out = world.fsdiff(&fork, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
}

#[test]
fn exits_with_2_for_a_directory_that_is_not_a_fork() {
    let world = World::new();
    let dir = world.project();
    let out = world.fsdiff(&dir, &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("is it a fork?"), "{}", stderr(&out));
    let out = world.fsdiff(&dir, &["missing"]);
    assert_eq!(out.status.code(), Some(2));
    let out = world.fsdiff(&dir, &["--no-such-option"]);
    assert_eq!(out.status.code(), Some(2));
    let out = world.fsdiff(&dir, &["a", "b", "c"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn compares_two_directories() {
    let world = World::new();
    let old = world.project();
    let new = world.path("new");
    write(&new, "a", "two\n");
    let out = world.fsdiff(&world.dir, &["project", "new"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let patch = stdout(&out);
    assert!(patch.contains("-one\n+two\n"), "{patch}");
    assert!(old.join("a").exists());
}

#[test]
fn leaves_out_git_directories_and_what_the_base_ignores() {
    let world = World::new();
    let dir = world.project();
    write(&dir, ".gitignore", "target/\n");
    let fork = world.fork(&dir);
    write(&fork, "target/out", "built\n");
    write(&fork, ".git/config", "config\n");
    write(&fork, "a", "two\n");
    let patch = stdout(&world.fsdiff(&fork, &[]));
    assert!(patch.contains("+two"), "{patch}");
    assert!(!patch.contains("target"), "{patch}");
    assert!(!patch.contains(".git/"), "{patch}");
}

#[test]
fn never_leaves_out_a_file_that_was_in_the_base() {
    let world = World::new();
    let dir = world.project();
    let fork = world.fork(&dir);
    write(&fork, ".gitignore", "a\n");
    write(&fork, "a", "two\n");
    let patch = stdout(&world.fsdiff(&fork, &[]));
    assert!(patch.contains("+two"), "{patch}");
    assert!(patch.contains("+++ b/.gitignore"), "{patch}");
}

#[test]
fn leaves_out_what_changed_in_the_directory_since_it_was_forked() {
    let world = World::new();
    let dir = world.project();
    let fork = world.fork(&dir);
    write(&fork, "b", "new\n");
    write(&dir, "a", "changed since\n");
    let patch = stdout(&world.fsdiff(&fork, &[]));
    assert!(patch.contains("+new"), "{patch}");
    assert!(!patch.contains("since"), "{patch}");
}

#[test]
fn shows_symbolic_links_as_links_without_following_them() {
    let world = World::new();
    let dir = world.project();
    write(&world.dir, "secret", "secret\n");
    let fork = world.fork(&dir);
    symlink("../secret", fork.join("link")).unwrap();
    let patch = stdout(&world.fsdiff(&fork, &[]));
    assert!(patch.contains("new file mode 120000"), "{patch}");
    assert!(patch.contains("+../secret"), "{patch}");
    assert!(!patch.contains("+secret"), "{patch}");
}

#[test]
fn the_patch_applies_to_the_directory() {
    let world = World::new();
    let dir = world.project();
    write(&dir, "c", "c\n");
    let fork = world.fork(&dir);
    write(&fork, "a", "two\n");
    write(&fork, "b", "new\n");
    fs::remove_file(fork.join("c")).unwrap();
    fs::write(fork.join("bin"), b"x\0y\xff").unwrap();
    write(&fork, "run", "#!/bin/sh\n");
    fs::set_permissions(fork.join("run"), fs::Permissions::from_mode(0o755)).unwrap();
    let patch = world.fsdiff(&fork, &[]).stdout;
    let mut apply = world
        .command("git", &dir)
        .arg("apply")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut apply.stdin.take().unwrap(), &patch).unwrap();
    assert!(apply.wait().unwrap().success());
    assert_eq!(read(&dir, "a"), "two\n");
    assert_eq!(read(&dir, "b"), "new\n");
    assert!(!dir.join("c").exists());
    assert_eq!(fs::read(dir.join("bin")).unwrap(), b"x\0y\xff");
    let mode = fs::metadata(dir.join("run")).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111);
}

#[test]
fn never_runs_anything_from_the_forks_repository() {
    let world = World::new();
    let dir = world.project();
    let fork = world.fork(&dir);
    let ran = world.path("ran");
    let hook = world.path("hook");
    fs::write(
        &hook,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", ran.display()),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    for args in [&["init", "--quiet"][..], &["config", "core.fsmonitor"]] {
        let mut git = world.command("git", &fork);
        git.args(args);
        if args[0] == "config" {
            git.arg(&hook);
        }
        assert!(git.status().unwrap().success());
    }
    write(&fork, "a", "two\n");
    // As the fork's repository is set up, git in it runs the hook.
    world.command("git", &fork).arg("status").output().unwrap();
    assert!(ran.exists(), "the hook does not run");
    fs::remove_file(&ran).unwrap();
    let out = world
        .fsdiff_command(&fork, &[])
        .env("GIT_DIR", fork.join(".git"))
        .env("GIT_WORK_TREE", &fork)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stdout(&out).contains("+two"), "{}", stdout(&out));
    assert!(!ran.exists(), "fsdiff ran the fork's hook");
}

/// A test's world: a home directory, and a directory to make projects and
/// forks in.
struct World {
    dir: PathBuf,
}

impl World {
    /// A new, empty world, named after the test.
    fn new() -> World {
        let name = thread::current()
            .name()
            .unwrap_or("test")
            .replace("::", "-");
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("fsdiff")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        World {
            dir: dir.canonicalize().unwrap(),
        }
    }

    /// A new directory, `project`, with a file `a` holding `one`.
    fn project(&self) -> PathBuf {
        let dir = self.path("project");
        write(&dir, "a", "one\n");
        dir
    }

    /// Forks `dir` as `fsfork` does, to `<dir>.1` with its base next to it.
    fn fork(&self, dir: &Path) -> PathBuf {
        let name = dir.file_name().unwrap().to_str().unwrap();
        let base = self.path(&format!(".{name}.1.base"));
        let fork = self.path(&format!("{name}.1"));
        for (from, to) in [(dir, &base), (&base, &fork)] {
            let copied = Command::new("cp").arg("-a").arg(from).arg(to).status();
            assert!(copied.unwrap().success());
        }
        fork
    }

    /// `name` in the world.
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// A command that runs `program` in `cwd` in the world: with its home
    /// directory, and git configured only by the repository, if any.
    fn command(&self, program: impl AsRef<std::ffi::OsStr>, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        command.current_dir(cwd);
        for (key, _) in env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        // The world is in this repository's `target`, which git would
        // otherwise take the world's directories to be in.
        command
            .env_remove("XDG_CONFIG_HOME")
            .env("GIT_CEILING_DIRECTORIES", &self.dir)
            .env("HOME", self.path("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_PAGER", "cat");
        command
    }

    /// A command that runs `fsdiff` with `args` in `cwd`.
    fn fsdiff_command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = self.command(env!("CARGO_BIN_EXE_fsdiff"), cwd);
        command.args(args);
        command
    }

    /// Runs `fsdiff` with `args` in `cwd`.
    fn fsdiff(&self, cwd: &Path, args: &[&str]) -> Output {
        self.fsdiff_command(cwd, args).output().unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // A failed test's world is kept, to look into.
        if !thread::panicking() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// Writes `contents` to `name` in `dir`.
fn write(dir: &Path, name: &str, contents: &str) {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// What `name` in `dir` holds.
fn read(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(name)).unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}
