//! Conformance tests: `confine` run as a user runs it, checked against what
//! `confine.1` says it does.
//!
//! `confine` needs a sandbox: on macOS `sandbox-exec`, which cannot run
//! inside another sandbox, such as that of a command already confined, and on
//! Linux Landlock, which may not be enabled. Tests are skipped where there is
//! none, and say so.
//!
//! Worlds are kept under Cargo's temporary directory for tests rather than
//! the system's, as the confined command may write to the latter.

use std::{
    env, fs,
    io::Write,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::OnceLock,
    thread,
};

/// Returns from the test, saying so, if the sandbox cannot run here.
macro_rules! need_sandbox {
    () => {
        if !sandbox() {
            eprintln!("skipped: the sandbox cannot run here");
            return;
        }
    };
}

#[test]
fn exits_with_the_commands_status() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let out = world.confine(&dir, &["sh", "-c", "exit 7"]);
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn is_ended_by_the_signal_that_ended_the_command() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let out = world.confine(&dir, &["sh", "-c", "kill -TERM $$"]);
    assert_eq!(out.status.signal(), Some(libc::SIGTERM));
}

#[test]
fn passes_everything_from_the_command_on_to_it() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let out = world.confine(&dir, &["echo", "--resume", "-w", "x"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "--resume -w x\n");
}

#[test]
fn runs_the_command_in_the_directory() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let out = world.confine(&world.dir, &["-d", "project", "pwd"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("{}\n", dir.display()));
}

#[test]
fn runs_the_command_in_a_fork_of_the_working_directory() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    write(&dir, "a", "a\n");
    let out = world.confine(&dir, &["sh", "-c", "pwd && cat a && echo b > b"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let fork = world.dir.join("project.1");
    assert_eq!(stdout(&out), format!("{}\na\n", fork.display()));
    assert!(stderr(&out).contains(&format!("forked to {}", fork.display())));
    assert_eq!(read(&fork, "b"), "b\n");
    assert!(!dir.join("b").exists());
    assert!(world.dir.join(".project.1.base").is_dir());
}

#[test]
fn the_command_in_a_fork_cannot_write_to_the_directory() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let out = world.confine(
        &dir,
        &["sh", "-c", "echo x > \"$1/f\"", "sh", dir.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(!dir.join("f").exists());
}

#[test]
fn removes_the_fork_after_the_command_with_rm() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let out = world.confine(&dir, &["--rm", "sh", "-c", "echo x > f; exit 7"]);
    assert_eq!(out.status.code(), Some(7), "{}", stderr(&out));
    let out = world.confine(&dir, &["--rm", "sh", "-c", "kill -TERM $$"]);
    assert_eq!(out.status.signal(), Some(libc::SIGTERM));
    assert_no_forks(&world);
}

#[test]
fn rm_cannot_be_given_with_d() {
    let world = World::new();
    let dir = world.dir("project");
    assert_fails(&world.confine(&dir, &["--rm", "-d", ".", "true"]), "--rm");
    assert!(dir.is_dir());
    assert_no_forks(&world);
}

#[test]
fn runs_the_shell_without_a_command() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let mut child = world
        .confine_command(&dir, &[])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"echo shell > f; exit 3\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert_eq!(read(&world.dir.join("project.1"), "f"), "shell\n");
}

#[test]
fn the_command_writes_only_to_the_directory() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let home = world.home();
    world.sh(&dir, "echo x > f && mkdir d && echo y > d/g");
    assert_eq!(read(&dir, "d/g"), "y\n");
    for path in [
        world.dir.join("escape"),
        home.join("escape"),
        home.join(".claude/x"),
    ] {
        let out = world.confine(
            &dir,
            &["sh", "-c", "echo x > \"$1\"", "sh", path.to_str().unwrap()],
        );
        assert!(!out.status.success(), "wrote {}", path.display());
        assert!(!path.exists());
    }
}

#[test]
fn the_command_writes_to_paths_given_with_w() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let claude = world.home().join(".claude");
    let other = world.dir("other");
    fs::create_dir(&claude).unwrap();
    let out = world.confine(
        &dir,
        &[
            "-w",
            claude.to_str().unwrap(),
            "--write",
            other.to_str().unwrap(),
            "sh",
            "-c",
            "mkdir \"$1/sessions\" && echo x > \"$1/sessions/s\" && echo y > \"$2/y\"",
            "sh",
            claude.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(read(&claude, "sessions/s"), "x\n");
    assert_eq!(read(&other, "y"), "y\n");
}

#[test]
fn the_command_writes_to_temporary_directories_and_caches() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    for cache in [".cargo", ".rustup", ".cache", ".npm", "Library/Caches"] {
        fs::create_dir_all(world.home().join(cache)).unwrap();
    }
    world.sh(
        &dir,
        "for d in /tmp \"${TMPDIR:-/tmp}\" ~/.cargo ~/.rustup ~/.cache ~/.npm ~/Library/Caches; do \
           echo x > \"$d/confine-test-$$\" && rm \"$d/confine-test-$$\" || exit 1; \
         done",
    );
}

#[test]
fn the_command_reads_only_what_it_needs_in_the_home_directory() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let home = world.home();
    for secret in [
        ".ssh/id_ed25519",
        ".aws/credentials",
        ".gnupg/key",
        ".config/gh/hosts.yml",
        ".netrc",
        ".npmrc",
        ".docker/config.json",
        ".config/other",
        "notes.txt",
    ] {
        write(&home, secret, "secret\n");
        let out = world.confine(&dir, &["cat", home.join(secret).to_str().unwrap()]);
        assert!(!out.status.success(), "read {secret}");
        assert!(!stdout(&out).contains("secret"));
    }
    for readable in [".gitconfig", ".config/git/ignore", ".cargo/config.toml"] {
        write(&home, readable, "readable\n");
        let out = world.confine(&dir, &["cat", home.join(readable).to_str().unwrap()]);
        assert_eq!(stdout(&out), "readable\n", "{readable}: {}", stderr(&out));
    }
}

#[test]
fn the_command_reads_paths_given_with_r() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let home = world.home();
    write(&home, ".npmrc", "readable\n");
    write(&home, "notes/a", "readable\n");
    let out = world.confine(
        &dir,
        &[
            "-r",
            home.join(".npmrc").to_str().unwrap(),
            "--read",
            home.join("notes").to_str().unwrap(),
            "sh",
            "-c",
            "cat ~/.npmrc ~/notes/a && ! echo x > ~/notes/b",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "readable\nreadable\n");
    assert!(!home.join("notes/b").exists());
}

#[test]
fn the_command_reads_the_directory_under_the_home_directory() {
    need_sandbox!();
    let world = World::new();
    let dir = world.home().join("src/project");
    write(&dir, "a", "a\n");
    let out = world.confine(&dir, &["sh", "-c", "pwd && cat a"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let fork = world.home().join("src/project.1");
    assert_eq!(stdout(&out), format!("{}\na\n", fork.display()));
}

#[test]
fn tells_claude_code_it_is_sandboxed() {
    need_sandbox!();
    let world = World::new();
    let dir = world.dir("project");
    let out = world.confine(&dir, &["sh", "-c", "echo $CLAUDE_CODE_SANDBOXED"]);
    assert_eq!(stdout(&out), "1\n");
}

#[test]
fn fails_for_paths_that_do_not_exist() {
    let world = World::new();
    let dir = world.dir("project");
    let missing = world.home().join("missing");
    assert_fails(&world.confine(&dir, &["-d", "missing", "true"]), "missing");
    assert_fails(
        &world.confine(&dir, &["-w", missing.to_str().unwrap(), "true"]),
        "missing",
    );
    assert_fails(
        &world.confine(&dir, &["-r", missing.to_str().unwrap(), "true"]),
        "missing",
    );
    write(&dir, "file", "");
    assert_fails(
        &world.confine(&dir, &["-d", "file", "true"]),
        "not a directory",
    );
    assert_no_forks(&world);
}

/// Whether `sandbox-exec` can run here.
#[cfg(target_os = "macos")]
fn sandbox() -> bool {
    static SANDBOX: OnceLock<bool> = OnceLock::new();
    *SANDBOX.get_or_init(|| {
        Command::new("/usr/bin/sandbox-exec")
            .args(["-p", "(version 1)(allow default)", "--", "/usr/bin/true"])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// Whether Landlock is enabled here, in a version `confine` can use.
#[cfg(target_os = "linux")]
fn sandbox() -> bool {
    static SANDBOX: OnceLock<bool> = OnceLock::new();
    *SANDBOX.get_or_init(|| {
        // `landlock_create_ruleset` with `LANDLOCK_CREATE_RULESET_VERSION`
        // returns the version of Landlock's ABI.
        let abi = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<u64>(),
                0,
                1,
            )
        };
        abi >= 2
    })
}

/// A test's world: a home directory, and directories to confine commands to.
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
            .join("confine")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        World {
            dir: dir.canonicalize().unwrap(),
        }
    }

    /// The home directory.
    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    /// A new, empty directory, `name`.
    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.dir.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A command that runs `confine` with `args` in `cwd`, with the world's
    /// home directory.
    fn confine_command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_confine"));
        command
            .args(args)
            .current_dir(cwd)
            .env("HOME", self.home())
            .env("SHELL", "/bin/sh");
        command
    }

    /// Runs `confine` with `args` in `cwd`.
    fn confine(&self, cwd: &Path, args: &[&str]) -> Output {
        self.confine_command(cwd, args).output().unwrap()
    }

    /// Runs `cmd` with `sh -c`, confined to `dir` itself, which must succeed.
    fn sh(&self, dir: &Path, cmd: &str) {
        let out = self.confine(dir, &["-d", ".", "sh", "-c", cmd]);
        assert!(out.status.success(), "{cmd}: {}", stderr(&out));
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

/// Asserts that `world` has no forks, nor bases, of its directories.
#[track_caller]
fn assert_no_forks(world: &World) {
    for entry in fs::read_dir(&world.dir).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy();
        assert!(
            !name.contains('.'),
            "{name} left in {}",
            world.dir.display()
        );
    }
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
