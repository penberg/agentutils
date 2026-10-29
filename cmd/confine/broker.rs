//! macOS: the broker, which runs `gh` for the confined command, outside the
//! sandbox, each time you allow it, so that the command can open pull
//! requests though it cannot read `gh`'s configuration.
//!
//! The command finds a shim first on its `PATH` in place of `gh`: `confine`
//! itself, under that name, which sends its arguments, working directory
//! and standard streams over a socket to the broker, and exits as the broker
//! says `gh` did. The broker asks you, with a dialog, whether to run it, and
//! runs the real `gh` with the shim's streams, so that it talks to the
//! terminal itself.
//!
//! `gh` runs git, and git runs what the repository's configuration and hooks
//! tell it to, both of which the command can write. So `gh` runs with a
//! clone of the repository's `.git` made where the command cannot write it,
//! and only if its configuration differs from what it was when `confine`
//! started in nothing but what `gh` and `git push` write themselves, with
//! hooks off. The clone then takes the place of `.git`, so that what `gh`
//! did to it, such as setting the branch it pushed to, is kept.

use std::{
    collections::HashSet,
    env,
    ffi::{OsStr, OsString},
    fs::{self, DirBuilder, File},
    io::{self, Read, Write},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{DirBuilderExt, PermissionsExt},
            net::{UnixListener, UnixStream},
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Path, PathBuf},
    process::{self, Command, ExitCode, Stdio},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use crate::at;

/// The variable that tells the shim where the broker's socket is.
const SOCKET: &str = "CONFINE_BROKER";

/// The variable that names a program to ask with instead of a dialog: it is
/// given the question, and prints `once` or `always` to allow, and anything
/// else to deny.
const ASK: &str = "CONFINE_ASK";

/// `gh`'s commands that manage credentials or configuration, or run
/// extensions, which are never run.
const DENIED: &[&str] = &[
    "alias",
    "auth",
    "config",
    "ext",
    "extension",
    "extensions",
    "gpg-key",
    "secret",
    "ssh-key",
    "variable",
];

/// `gh`'s commands that can do anything, which you are asked about every
/// time.
const UNBOUNDED: &[&str] = &["api"];

/// The longest argument the shim may send, to bound what the broker reads.
const MAX_LEN: u64 = 1 << 24;

/// Runs `gh` for a confined command, until it is dropped.
pub struct Broker {
    /// Where the shim and the socket are, which the command may write to.
    shared: PathBuf,
    /// Where the clones of `.git` are made, which the command may not write
    /// to, or read.
    private: PathBuf,
}

/// What the broker knows, which each request it serves shares.
struct State {
    /// The real `gh`.
    gh: PathBuf,
    /// The directory the command is confined to.
    dir: PathBuf,
    private: PathBuf,
    /// The configuration of the directory's `.git` when `confine` started,
    /// if it was a repository.
    config: Option<HashSet<Vec<u8>>>,
    /// The commands you allowed always, such as `gh pr create`, which is
    /// locked while you are asked, so that you are asked one thing at a time.
    always: Mutex<HashSet<String>>,
    /// How many clones of `.git` have been made, to name the next.
    clones: AtomicU64,
}

/// What you answered.
enum Answer {
    Deny,
    Once,
    Always,
}

impl Broker {
    /// Starts brokering `gh` for a command confined to `dir`, in which `home`
    /// is the home directory, unless `gh` is not installed.
    pub fn start(dir: &Path, home: &Path) -> io::Result<Option<Broker>> {
        let Some(gh) = find("gh") else {
            return Ok(None);
        };
        let git = dir.join(".git");
        let config = if git.is_dir() {
            Some(config(&git)?)
        } else {
            None
        };
        let id = format!("confine-{}", process::id());
        let shared = env::temp_dir().canonicalize()?.join(&id);
        DirBuilder::new().mode(0o700).create(&shared)?;
        let private = home.join(".local/state/confine").join(&id);
        let broker = Broker { shared, private };
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&broker.private)?;
        fs::create_dir(broker.shared.join("bin"))?;
        fs::copy(env::current_exe()?, broker.shared.join("bin/gh"))?;
        let listener = UnixListener::bind(broker.socket())?;
        let state = Arc::new(State {
            gh,
            dir: dir.to_path_buf(),
            private: broker.private.clone(),
            config,
            always: Mutex::new(HashSet::new()),
            clones: AtomicU64::new(0),
        });
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = state.clone();
                thread::spawn(move || state.serve(stream));
            }
        });
        Ok(Some(broker))
    }

    /// The environment the command needs for `gh` to be the shim.
    pub fn env(&self) -> io::Result<Vec<(&'static str, OsString)>> {
        let mut paths = vec![self.shared.join("bin")];
        paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
        let path = env::join_paths(paths).map_err(io::Error::other)?;
        Ok(vec![("PATH", path), (SOCKET, self.socket().into())])
    }

    fn socket(&self) -> PathBuf {
        self.shared.join("socket")
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.shared);
        let _ = fs::remove_dir_all(&self.private);
    }
}

impl State {
    /// Serves the shim at the other end of `stream`: runs `gh` for it, if
    /// you allow it, and tells it how `gh` exited.
    fn serve(&self, mut stream: UnixStream) {
        let Ok(Ok([stdin, stdout, stderr])) = receive_fds(&stream).map(<[OwnedFd; 3]>::try_from)
        else {
            return;
        };
        let Ok(mut err) = stderr.try_clone().map(File::from) else {
            return;
        };
        let status = read_strings(&mut stream)
            .and_then(|request| self.run(request, [stdin, stdout, stderr], &mut err))
            .unwrap_or_else(|e| {
                let _ = writeln!(err, "confine: {e}");
                1
            });
        let _ = stream.write_all(&status.to_be_bytes());
    }

    /// Runs `gh` as `request`, its working directory and then its
    /// arguments, asks, with `stdio`, if you allow it, and returns its exit
    /// status.
    fn run(&self, request: Vec<OsString>, stdio: [OwnedFd; 3], err: &mut File) -> io::Result<i32> {
        let mut request = request.into_iter();
        let cwd = PathBuf::from(request.next().unwrap_or_default());
        let args: Vec<OsString> = request.collect();
        let cwd = cwd.canonicalize().map_err(|e| at(&cwd, e))?;
        if !cwd.starts_with(&self.dir) {
            return Err(io::Error::other(format!(
                "{}: gh runs only in {}",
                cwd.display(),
                self.dir.display()
            )));
        }
        let Some(args) = args
            .iter()
            .map(|arg| arg.to_str())
            .collect::<Option<Vec<&str>>>()
        else {
            return Err(io::Error::other("gh's arguments must be UTF-8"));
        };
        // The command, such as `gh pr create`: the words before the first
        // flag, up to two.
        let words: Vec<&str> = args
            .iter()
            .take_while(|arg| !arg.starts_with('-'))
            .take(2)
            .copied()
            .collect();
        if let Some(word) = words.first().filter(|word| DENIED.contains(word)) {
            return Err(io::Error::other(format!("gh {word}: never run for it")));
        }
        let line = quoted(&args);
        let always = (!words.is_empty() && !UNBOUNDED.contains(&words[0]))
            .then(|| format!("gh {}", words.join(" ")));
        if !self.allowed(&line, &cwd, always)? {
            return Err(io::Error::other(format!("{line}: you did not allow it")));
        }
        let git = self.clone_git()?;
        let [stdin, stdout, stderr] = stdio;
        let mut gh = Command::new(&self.gh);
        gh.args(&args)
            .current_dir(&cwd)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            .env(
                "GIT_DIR",
                git.clone().unwrap_or_else(|| self.private.join("none")),
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
            gh.env(format!("GIT_CONFIG_KEY_{}", n + i), key)
                .env(format!("GIT_CONFIG_VALUE_{}", n + i), value);
        }
        gh.env("GIT_CONFIG_COUNT", (n + off.len()).to_string());
        // `confine` ignores the terminal's signals while it waits, which `gh`
        // gets as they are by default.
        unsafe {
            gh.pre_exec(|| {
                for signal in crate::SIGNALS {
                    libc::signal(signal, libc::SIG_DFL);
                }
                Ok(())
            });
        }
        let status = gh.status().map_err(|e| at(&self.gh, e));
        if let Some(clone) = git
            && let Err(e) = self.restore(&clone)
        {
            let _ = writeln!(err, "confine: what gh changed in .git is lost: {e}");
            let _ = fs::remove_dir_all(&clone);
        }
        let status = status?;
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
    }

    /// Whether you allow running `line` in `cwd`: once, or, if `always` is
    /// given, every time the command runs it until `confine` exits.
    fn allowed(&self, line: &str, cwd: &Path, always: Option<String>) -> io::Result<bool> {
        let mut allowed = self.always.lock().unwrap_or_else(|e| e.into_inner());
        if always
            .as_ref()
            .is_some_and(|always| allowed.contains(always))
        {
            return Ok(true);
        }
        let question = format!(
            "The command confined to {} wants to run:\n\n{line}\n\nin {}",
            self.dir.display(),
            cwd.display()
        );
        let label = always
            .as_ref()
            .map(|always| format!("Always allow {always}"));
        Ok(match ask(&question, label.as_deref())? {
            Answer::Deny => false,
            Answer::Once => true,
            Answer::Always => {
                allowed.extend(always);
                true
            }
        })
    }

    /// Clones the directory's `.git` to where the command cannot write, and
    /// returns where, if its configuration is safe for git to run with. If
    /// the directory was not a repository, there is none.
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

    /// Puts `clone` in the place of the directory's `.git`.
    fn restore(&self, clone: &Path) -> io::Result<()> {
        let git = self.dir.join(".git");
        let old = clone.with_extension("old");
        fs::rename(&git, &old)?;
        if let Err(e) = fs::rename(clone, &git) {
            let _ = fs::rename(&old, &git);
            return Err(e);
        }
        fs::remove_dir_all(&old)
    }
}

/// Checks that the configuration of `git` differs from `original` in
/// nothing but what `gh` and `git push` write, and that it does not name
/// another directory to take it from.
fn check(git: &Path, original: &HashSet<Vec<u8>>) -> io::Result<()> {
    if git.join("commondir").symlink_metadata().is_ok() {
        return Err(io::Error::other(
            ".git/commondir: made by the command, so gh is not run",
        ));
    }
    for entry in config(git)? {
        if !original.contains(&entry) && !written_by_gh(&entry) {
            let entry = String::from_utf8_lossy(&entry);
            let key = entry.split('\n').next().unwrap_or_default();
            return Err(io::Error::other(format!(
                ".git/config: {key} changed by the command, so gh is not run"
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

/// Asks you `question`, with a dialog, or the program `CONFINE_ASK` names,
/// offering to allow once, and, if `always` is given, with it as the label.
fn ask(question: &str, always: Option<&str>) -> io::Result<Answer> {
    if let Some(program) = env::var_os(ASK) {
        let out = Command::new(&program).arg(question).output()?;
        return Ok(match String::from_utf8_lossy(&out.stdout).trim() {
            "once" => Answer::Once,
            "always" if always.is_some() => Answer::Always,
            "always" => Answer::Once,
            _ => Answer::Deny,
        });
    }
    // The question and the label are arguments, so they need no quoting.
    // Deny is the default, and what a dialog left unanswered gives.
    let script = r#"on run argv
        set choices to {"Deny", "Allow once"}
        if (count of argv) > 1 then set end of choices to item 2 of argv
        display dialog (item 1 of argv) with title "confine" buttons choices ¬
            default button "Deny" cancel button "Deny" with icon caution ¬
            giving up after 300
        return button returned of result
    end run"#;
    let out = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .arg(question)
        .args(always)
        .stderr(Stdio::null())
        .output()?;
    let answer = String::from_utf8_lossy(&out.stdout);
    Ok(match answer.trim() {
        "Allow once" => Answer::Once,
        answer if Some(answer) == always => Answer::Always,
        _ => Answer::Deny,
    })
}

/// `gh` and `args` as a shell would take them, with what is not plain
/// quoted, and control characters escaped, so that you see each argument as
/// it is.
fn quoted(args: &[&str]) -> String {
    let mut line = String::from("gh");
    for arg in args {
        line.push(' ');
        let plain = |b: u8| b.is_ascii_alphanumeric() || b"%+,-./:=@_".contains(&b);
        if !arg.is_empty() && arg.bytes().all(plain) {
            line.push_str(arg);
        } else if arg.chars().any(char::is_control) {
            line.push_str(&format!("$'{}'", arg.escape_default()));
        } else {
            line.push_str(&format!("'{}'", arg.replace('\'', r"'\''")));
        }
    }
    line
}

/// Whether this process is the shim: `confine` run as `gh` by a confined
/// command.
pub fn is_shim() -> bool {
    env::var_os(SOCKET).is_some()
        && env::args_os()
            .next()
            .is_some_and(|arg0| Path::new(&arg0).file_name() == Some(OsStr::new("gh")))
}

/// Runs as the shim: has the broker run `gh`, and exits as it did.
pub fn shim() -> ExitCode {
    match request() {
        Ok(status) => ExitCode::from(status as u8),
        Err(e) => {
            eprintln!("gh: confine: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Sends the broker the standard streams, the working directory and the
/// arguments, and returns the exit status it sends back.
fn request() -> io::Result<i32> {
    let mut stream = UnixStream::connect(env::var_os(SOCKET).unwrap_or_default())?;
    send_fds(&stream, &[0, 1, 2])?;
    let mut request = vec![env::current_dir()?.into_os_string()];
    request.extend(env::args_os().skip(1));
    write_strings(&mut stream, &request)?;
    let mut status = [0; 4];
    stream.read_exact(&mut status)?;
    Ok(i32::from_be_bytes(status))
}

/// Sends `fds` over `stream`, with a byte, as they can be sent only with
/// data.
fn send_fds(stream: &UnixStream, fds: &[RawFd]) -> io::Result<()> {
    let byte = 0u8;
    let mut iov = libc::iovec {
        iov_base: ptr::from_ref(&byte).cast_mut().cast(),
        iov_len: 1,
    };
    let len = mem::size_of_val(fds) as u32;
    let space = unsafe { libc::CMSG_SPACE(len) } as usize;
    // Of `u64`s, for the alignment of a `cmsghdr`.
    let mut control = vec![0u64; space.div_ceil(8)];
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = space as _;
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(len) as _;
        ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cmsg).cast(), fds.len());
        if libc::sendmsg(stream.as_raw_fd(), &msg, 0) != 1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Receives the file descriptors `send_fds` sends over `stream`.
fn receive_fds(stream: &UnixStream) -> io::Result<Vec<OwnedFd>> {
    let mut byte = 0u8;
    let mut iov = libc::iovec {
        iov_base: ptr::from_mut(&mut byte).cast(),
        iov_len: 1,
    };
    let space = unsafe { libc::CMSG_SPACE(3 * size_of::<RawFd>() as u32) } as usize;
    let mut control = vec![0u64; space.div_ceil(8)];
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = space as _;
    if unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut fds = Vec::new();
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let count =
                    ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / size_of::<RawFd>();
                let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                for i in 0..count {
                    let fd = data.add(i).read_unaligned();
                    // So that the `gh` run for another request gets none of
                    // these. macOS has no flag to receive them so.
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                    fds.push(OwnedFd::from_raw_fd(fd));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::other("too many file descriptors"));
    }
    Ok(fds)
}

/// Writes `strings`, their count and then each, its length first.
fn write_strings(stream: &mut UnixStream, strings: &[OsString]) -> io::Result<()> {
    let mut buf = (strings.len() as u32).to_be_bytes().to_vec();
    for string in strings {
        buf.extend((string.len() as u32).to_be_bytes());
        buf.extend(string.as_bytes());
    }
    stream.write_all(&buf)
}

/// Reads what `write_strings` writes.
fn read_strings(stream: &mut UnixStream) -> io::Result<Vec<OsString>> {
    let count = read_u32(stream)?;
    let mut strings = Vec::new();
    for _ in 0..count {
        let len = u64::from(read_u32(stream)?);
        if len > MAX_LEN {
            return Err(io::Error::other("an argument is too long"));
        }
        let mut string = Vec::new();
        stream.take(len).read_to_end(&mut string)?;
        if string.len() as u64 != len {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        strings.push(OsString::from_vec(string));
    }
    Ok(strings)
}

fn read_u32(stream: &mut UnixStream) -> io::Result<u32> {
    let mut buf = [0; 4];
    stream.read_exact(&mut buf)?;
    Ok(u32::from_be_bytes(buf))
}

/// The first executable file named `name` in a directory on `PATH`.
fn find(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(name))
        .find(|path| {
            fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
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
    fn quotes_what_is_not_plain() {
        assert_eq!(
            quoted(&["pr", "create", "--title", "it's", "a\nb", ""]),
            r"gh pr create --title 'it'\''s' $'a\nb' ''"
        );
    }

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
