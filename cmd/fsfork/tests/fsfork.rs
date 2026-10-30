//! Conformance tests: `fsfork` run as a user runs it, checked against what
//! `fsfork.1` says it does.

use std::{
    env, fs,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt, symlink},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    process::{self, Command, Output},
    thread,
};

#[test]
fn forks_the_working_directory_next_to_it() {
    let world = World::new();
    let dir = world.project();
    let out = fsfork(&dir, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let fork = world.path("project.1");
    assert_eq!(stdout(&out), format!("{}\n", fork.display()));
    assert_eq!(read(&fork, "a"), "one\n");
    assert_eq!(read(&world.path(".project.1.base"), "a"), "one\n");
}

#[test]
fn numbers_forks_with_the_first_number_whose_fork_and_base_are_free() {
    let world = World::new();
    let dir = world.project();
    for n in 1..=3 {
        let out = fsfork(&dir, &[]);
        let fork = world.path(&format!("project.{n}"));
        assert_eq!(stdout(&out), format!("{}\n", fork.display()));
    }
    // A fork removed without its base keeps its number taken.
    fs::remove_dir_all(world.path("project.2")).unwrap();
    let out = fsfork(&dir, &[]);
    assert_eq!(
        stdout(&out),
        format!("{}\n", world.path("project.4").display())
    );
    fs::remove_dir_all(world.path(".project.2.base")).unwrap();
    let out = fsfork(&dir, &[]);
    assert_eq!(
        stdout(&out),
        format!("{}\n", world.path("project.2").display())
    );
}

#[test]
fn forks_the_directory_given() {
    let world = World::new();
    world.project();
    let out = fsfork(&world.dir, &["project"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("{}\n", world.path("project.1").display())
    );
}

#[test]
fn forks_to_the_path_given() {
    let world = World::new();
    world.project();
    let out = fsfork(&world.dir, &["project", "parser"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let fork = world.path("parser");
    assert_eq!(stdout(&out), format!("{}\n", fork.display()));
    assert_eq!(read(&fork, "a"), "one\n");
    assert_eq!(read(&world.path(".parser.base"), "a"), "one\n");
}

#[test]
fn copies_everything_in_the_directory_as_it_is() {
    let world = World::new();
    let dir = world.project();
    write(&dir, ".hidden", "hidden\n");
    write(&dir, ".git/config", "config\n");
    write(&dir, "target/out", "built\n");
    write(&dir, "run", "#!/bin/sh\n");
    fs::set_permissions(dir.join("run"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("a", dir.join("link")).unwrap();
    let fork = PathBuf::from(stdout(&fsfork(&dir, &[])).trim_end());
    assert_eq!(read(&fork, ".hidden"), "hidden\n");
    assert_eq!(read(&fork, ".git/config"), "config\n");
    assert_eq!(read(&fork, "target/out"), "built\n");
    let mode = fs::metadata(fork.join("run")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(fs::read_link(fork.join("link")).unwrap(), Path::new("a"));
}

#[test]
fn leaves_out_what_gitignore_files_ignore_with_skip_ignored() {
    let world = World::new();
    let dir = world.project();
    write(&dir, ".gitignore", "target/\n*.log\nbuild/*\n!build/keep\n");
    write(&dir, "target/deep/out", "built\n");
    write(&dir, "kept/b", "two\n");
    write(&dir, "kept/.gitignore", "only-here\n");
    write(&dir, "kept/only-here", "ignored\n");
    write(&dir, "kept/run.log", "ignored\n");
    write(&dir, "build/keep", "kept\n");
    write(&dir, "build/out", "ignored\n");
    fs::create_dir(dir.join("empty")).unwrap();
    symlink("a", dir.join("kept/link")).unwrap();
    fs::set_permissions(dir.join("kept"), fs::Permissions::from_mode(0o750)).unwrap();
    let out = fsfork(&dir, &["--skip-ignored"]);
    assert!(out.status.success(), "{}", stderr(&out));
    for tree in [world.path("project.1"), world.path(".project.1.base")] {
        assert_eq!(read(&tree, "a"), "one\n");
        assert_eq!(
            read(&tree, ".gitignore"),
            "target/\n*.log\nbuild/*\n!build/keep\n"
        );
        assert_eq!(read(&tree, "kept/b"), "two\n");
        assert_eq!(read(&tree, "build/keep"), "kept\n");
        assert_eq!(
            fs::read_link(tree.join("kept/link")).unwrap(),
            Path::new("a")
        );
        assert!(tree.join("empty").is_dir());
        assert!(!tree.join("target").exists());
        assert!(!tree.join("kept/only-here").exists());
        assert!(!tree.join("kept/run.log").exists());
        assert!(!tree.join("build/out").exists());
        let mode = fs::metadata(tree.join("kept"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o750);
    }
    assert_eq!(read(&dir, "target/deep/out"), "built\n");
}

#[test]
fn keeps_tracked_files_and_the_repository_with_skip_ignored() {
    let world = World::new();
    let dir = world.project();
    git(&dir, &["init", "--quiet"]);
    write(&dir, ".gitignore", "*\n!.gitignore\n");
    write(&dir, "tracked/forced", "tracked\n");
    write(&dir, "tracked/other", "ignored\n");
    git(&dir, &["add", ".gitignore"]);
    git(&dir, &["add", "--force", "tracked/forced"]);
    let fork = PathBuf::from(stdout(&fsfork(&dir, &["--skip-ignored"])).trim_end());
    assert_eq!(read(&fork, "tracked/forced"), "tracked\n");
    assert!(!fork.join("tracked/other").exists());
    assert!(!fork.join("a").exists());
    assert!(fork.join(".git/config").exists());
}

#[test]
fn skips_what_is_ignored_in_a_subdirectory_and_a_worktree_of_a_repository() {
    let world = World::new();
    let dir = world.project();
    git(&dir, &["init", "--quiet"]);
    write(&dir, ".gitignore", "*.o\n");
    write(&dir, "sub/x.o", "ignored\n");
    write(&dir, "sub/forced.o", "tracked\n");
    write(&dir, "sub/c", "c\n");
    git(&dir, &["add", "."]);
    git(&dir, &["add", "--force", "sub/forced.o"]);
    git(
        &dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "c",
        ],
    );
    let fork = PathBuf::from(stdout(&fsfork(&dir.join("sub"), &["--skip-ignored"])).trim_end());
    assert_eq!(read(&fork, "c"), "c\n");
    assert_eq!(read(&fork, "forced.o"), "tracked\n");
    assert!(!fork.join("x.o").exists());
    let worktree = world.path("worktree");
    git(
        &dir,
        &["worktree", "add", "--quiet", worktree.to_str().unwrap()],
    );
    write(&worktree, "sub/y.o", "ignored\n");
    let fork = PathBuf::from(stdout(&fsfork(&worktree, &["--skip-ignored"])).trim_end());
    assert_eq!(read(&fork, "sub/forced.o"), "tracked\n");
    assert!(!fork.join("sub/y.o").exists());
}

#[test]
fn skip_ignored_takes_fifos_and_sockets_beside_what_is_ignored() {
    let world = World::new();
    let dir = world.project();
    write(&dir, ".gitignore", "target/\n");
    write(&dir, "target/out", "built\n");
    let made = Command::new("mkfifo")
        .arg(dir.join("pipe"))
        .status()
        .unwrap();
    assert!(made.success());
    // The path to a socket is limited in length, which the world's exceeds.
    let short = env::temp_dir().join(format!("fsfork-test.{}", process::id()));
    symlink(&dir, &short).unwrap();
    let socket = UnixListener::bind(short.join("sock"));
    fs::remove_file(&short).unwrap();
    let _socket = socket.unwrap();
    let out = fsfork(&dir, &["--skip-ignored"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let fork = world.path("project.1");
    let pipe = fork.join("pipe").symlink_metadata().unwrap();
    assert!(pipe.file_type().is_fifo());
    assert_eq!(read(&fork, "a"), "one\n");
}

#[test]
fn a_directory_that_is_ignored_itself_goes_by_its_own_gitignore_files() {
    let world = World::new();
    let dir = world.project();
    git(&dir, &["init", "--quiet"]);
    write(&dir, ".gitignore", "vendor/\n");
    write(&dir, "vendor/lib/.gitignore", "out/\n");
    write(&dir, "vendor/lib/src", "src\n");
    write(&dir, "vendor/lib/out/o", "built\n");
    let lib = dir.join("vendor/lib");
    let fork = PathBuf::from(stdout(&fsfork(&lib, &["--skip-ignored"])).trim_end());
    assert_eq!(read(&fork, "src"), "src\n");
    assert!(!fork.join("out").exists());
}

#[test]
fn skip_ignored_does_not_run_what_the_repository_configures() {
    let world = World::new();
    let dir = world.project();
    git(&dir, &["init", "--quiet"]);
    write(&dir, ".gitignore", "target/\n");
    write(&dir, "target/out", "built\n");
    let ran = world.path("ran");
    let monitor = world.path("monitor");
    write(
        &world.dir,
        "monitor",
        &format!("#!/bin/sh\ntouch {}\n", ran.display()),
    );
    fs::set_permissions(&monitor, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &dir,
        &["config", "core.fsmonitor", monitor.to_str().unwrap()],
    );
    let out = fsfork(&dir, &["--skip-ignored"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!world.path("project.1/target").exists());
    assert!(!ran.exists());
}

#[test]
fn fails_with_skip_ignored_if_the_fork_or_its_base_exists() {
    let world = World::new();
    let dir = world.project();
    write(&dir, ".gitignore", "target/\n");
    write(&dir, "target/out", "built\n");
    write(&world.path("parser"), "mine", "mine\n");
    write(&world.path(".lexer.base"), "mine", "mine\n");
    let args = |fork| ["--skip-ignored", "project", fork];
    assert_fails(&fsfork(&world.dir, &args("parser")), "exists");
    assert_eq!(read(&world.path("parser"), "mine"), "mine\n");
    assert!(!world.path(".parser.base").exists());
    assert_fails(&fsfork(&world.dir, &args("lexer")), "exists");
    assert_eq!(read(&world.path(".lexer.base"), "mine"), "mine\n");
    assert!(!world.path("lexer").exists());
}

#[test]
fn the_directory_the_fork_and_the_base_change_apart() {
    let world = World::new();
    let dir = world.project();
    let fork = PathBuf::from(stdout(&fsfork(&dir, &[])).trim_end());
    let base = world.path(".project.1.base");
    write(&fork, "a", "fork\n");
    write(&dir, "b", "dir\n");
    assert_eq!(read(&dir, "a"), "one\n");
    assert_eq!(read(&base, "a"), "one\n");
    assert!(!fork.join("b").exists());
    assert!(!base.join("b").exists());
}

#[test]
fn fails_if_the_fork_or_its_base_exists() {
    let world = World::new();
    world.project();
    fs::create_dir(world.path("parser")).unwrap();
    assert_fails(&fsfork(&world.dir, &["project", "parser"]), "exists");
    assert!(!world.path(".parser.base").exists());
    fs::create_dir(world.path(".lexer.base")).unwrap();
    assert_fails(&fsfork(&world.dir, &["project", "lexer"]), "exists");
    assert!(!world.path("lexer").exists());
}

#[test]
fn fails_to_fork_into_the_directory() {
    let world = World::new();
    let dir = world.project();
    assert_fails(&fsfork(&dir, &[".", "inside"]), "cannot be inside");
    assert!(!dir.join("inside").exists());
}

#[test]
fn fails_for_what_is_not_a_directory() {
    let world = World::new();
    world.project();
    assert_fails(&fsfork(&world.dir, &["missing"]), "missing");
    assert_fails(&fsfork(&world.dir, &["project/a"]), "not a directory");
    assert_fails(&fsfork(&world.dir, &["a", "b", "c"]), "too many arguments");
}

/// A test's world: a directory to make projects and forks in.
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
            .join("fsfork")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
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

    /// `name` in the world.
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
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

/// Runs `fsfork` with `args` in `cwd`.
fn fsfork(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fsfork"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

/// Runs git with `args` in `dir`, which must succeed.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
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

/// Asserts that `out` is of a command that failed with status 1, saying
/// something with `message` in it.
#[track_caller]
fn assert_fails(out: &Output, message: &str) {
    assert_eq!(out.status.code(), Some(1), "{}", stderr(out));
    assert!(
        stderr(out).contains(message),
        "expected {message:?} in: {}",
        stderr(out)
    );
}
