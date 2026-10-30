//! macOS: the broker, which runs tools such as `gh` for the confined
//! command, outside the sandbox, each time you allow it, so that the command
//! can open pull requests though it cannot read `gh`'s configuration.
//!
//! The command finds a shim first on its `PATH` in place of each tool:
//! `confine` itself, under the tool's name, which sends its arguments,
//! working directory and standard streams over a socket to the broker, and
//! exits as the broker says the tool did. The broker asks you, with a
//! dialog that denies by itself if no one answers, whether to run it, and
//! runs the real tool with the shim's streams, so that it talks to the
//! terminal itself. What each tool may be asked to do, and how it is kept
//! from running what the command wrote, is in `tools`.

use std::{
    collections::HashSet,
    env,
    ffi::OsString,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{DirBuilderExt, PermissionsExt, symlink},
            net::{UnixListener, UnixStream},
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Path, PathBuf},
    process::{self, Command, ExitCode, Stdio},
    ptr,
    sync::{Arc, Mutex},
    thread,
};

use crate::{
    at,
    tools::{Policy, Repo, TOOLS, Tool},
};

/// The variable that tells the shim where the broker's socket is.
const SOCKET: &str = "CONFINE_SOCKET";

/// The variable that names a program to ask with instead of a dialog: it is
/// given the question, and prints `once` or `always` to allow, and anything
/// else to deny.
const ASK: &str = "CONFINE_ASK";

/// The file, in the home directory, that lists what you allowed always,
/// one per line, such as `gh pr create`. The command can neither read nor
/// write it.
const ALLOWED: &str = ".config/confine/allow";

/// How long the dialog waits for an answer before it denies, in seconds, so
/// that a command no one is watching is not kept waiting.
const TIMEOUT: u32 = 10;

/// The dialog, in JavaScript for Automation: an alert that asks whether to
/// let the tool run outside the sandbox, with the command line in a box of
/// its own, and whose default button, Deny, counts down, and is pressed when
/// it reaches zero. It is given the tool, the command line, the directory the
/// command is confined to, the one the tool is to run in, the time out, and,
/// to offer to allow always, what to allow so, and prints `once`, `always`
/// or `deny`.
const DIALOG: &str = r#"ObjC.import('Cocoa');
function run(argv) {
    const [tool, line, dir, cwd, timeout, always] =
        [argv[0], argv[1], argv[2], argv[3], Number(argv[4]), argv[5]];
    const tilde = path => $(path).stringByAbbreviatingWithTildeInPath.js;
    const app = $.NSApplication.sharedApplication;
    app.setActivationPolicy($.NSApplicationActivationPolicyAccessory);
    const alert = $.NSAlert.alloc.init;
    alert.messageText = `Allow ${tool} to run outside the sandbox?`;
    alert.informativeText = cwd == dir
        ? `The command confined to ${tilde(dir)} wants to run:`
        : `The command confined to ${tilde(dir)} wants to run, in ${tilde(cwd)}:`;
    const symbol = $.NSImage.imageWithSystemSymbolNameAccessibilityDescription('lock.shield', 'confine');
    if (symbol && !symbol.isNil()) {
        const config = $.NSImageSymbolConfiguration.configurationWithPointSizeWeight(48, $.NSFontWeightRegular)
            .configurationByApplyingConfiguration(
                $.NSImageSymbolConfiguration.configurationWithHierarchicalColor($.NSColor.systemOrangeColor));
        alert.icon = symbol.imageWithSymbolConfiguration(config);
    }
    // The command line, monospaced, wrapped, and selectable, on a rounded
    // background, wide enough that the buttons sit in a row.
    const width = 420, pad = 10;
    const text = $.NSTextField.wrappingLabelWithString(line);
    text.font = $.NSFont.monospacedSystemFontOfSizeWeight(12, $.NSFontWeightRegular);
    text.selectable = true;
    const size = text.cell.cellSizeForBounds($.NSMakeRect(0, 0, width - 2 * pad, 10000));
    const height = Math.min(size.height, 200) + 2 * pad;
    text.frame = $.NSMakeRect(pad, pad, width - 2 * pad, height - 2 * pad);
    const box = $.NSView.alloc.initWithFrame($.NSMakeRect(0, 0, width, height));
    box.wantsLayer = true;
    box.layer.cornerRadius = 6;
    box.layer.backgroundColor = $.NSColor.quaternaryLabelColor.CGColor;
    box.addSubview(text);
    alert.accessoryView = box;
    const deny = alert.addButtonWithTitle(`Deny (${timeout})`);
    alert.addButtonWithTitle('Allow Once');
    if (always) alert.addButtonWithTitle(`Always Allow “${always}”`);
    let left = timeout;
    const timer = $.NSTimer.timerWithTimeIntervalRepeatsBlock(1, true, t => {
        left -= 1;
        if (left > 0) {
            deny.title = `Deny (${left})`;
        } else {
            t.invalidate;
            app.abortModal;
        }
    });
    $.NSRunLoop.currentRunLoop.addTimerForMode(timer, $.NSModalPanelRunLoopMode);
    app.activateIgnoringOtherApps(true);
    alert.window.initialFirstResponder = deny;
    const response = alert.runModal;
    timer.invalidate;
    return response == $.NSAlertSecondButtonReturn ? 'once'
        : response == $.NSAlertThirdButtonReturn ? 'always'
        : 'deny';
}"#;

/// The longest argument the shim may send, to bound what the broker reads.
const MAX_LEN: u64 = 1 << 24;

/// Runs tools for a confined command, until it is dropped.
pub struct Broker {
    /// Where the shims and the socket are, which the command may write to.
    shared: PathBuf,
    /// Where the clones of `.git` are made, which the command may not write
    /// to, or read.
    private: PathBuf,
}

/// What the broker knows, which each request it serves shares.
struct State {
    /// The tools that are installed, and where.
    tools: Vec<(&'static dyn Tool, PathBuf)>,
    /// The directory the command is confined to.
    dir: PathBuf,
    repo: Repo,
    /// The file that lists what you allowed always.
    allowed: PathBuf,
    /// Locked while you are asked, so that you are asked one thing at a
    /// time.
    asking: Mutex<()>,
}

/// What you answered.
enum Answer {
    Deny,
    Once,
    Always,
}

impl Broker {
    /// Starts running tools for a command confined to `dir`, in which
    /// `home` is the home directory, unless none is installed.
    pub fn start(dir: &Path, home: &Path) -> io::Result<Option<Broker>> {
        let tools: Vec<(&'static dyn Tool, PathBuf)> = TOOLS
            .iter()
            .filter_map(|&tool| find(tool.name()).map(|path| (tool, path)))
            .collect();
        if tools.is_empty() {
            return Ok(None);
        }
        let id = format!("confine-{}", process::id());
        let shared = env::temp_dir().canonicalize()?.join(&id);
        DirBuilder::new().mode(0o700).create(&shared)?;
        let private = home.join(".local/state/confine").join(&id);
        let broker = Broker { shared, private };
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&broker.private)?;
        let repo = Repo::new(dir, &broker.private)?;
        // The command may not read `confine` where it is installed, so a
        // copy is where it can.
        fs::copy(env::current_exe()?, broker.shared.join("confine"))?;
        fs::create_dir(broker.shared.join("bin"))?;
        for (tool, _) in &tools {
            symlink("../confine", broker.shared.join("bin").join(tool.name()))?;
        }
        let listener = UnixListener::bind(broker.socket())?;
        let state = Arc::new(State {
            tools,
            dir: dir.to_path_buf(),
            repo,
            allowed: home.join(ALLOWED),
            asking: Mutex::new(()),
        });
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = state.clone();
                thread::spawn(move || state.serve(stream));
            }
        });
        Ok(Some(broker))
    }

    /// The environment the command needs for the tools to be the shims.
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
    /// Serves the shim at the other end of `stream`: runs its tool for it,
    /// if you allow it, and tells it how the tool exited.
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

    /// Runs the tool as `request`, its name, its working directory and then
    /// its arguments, says, if you allow it, with `stdio`, and returns its
    /// exit status.
    fn run(&self, request: Vec<OsString>, stdio: [OwnedFd; 3], err: &mut File) -> io::Result<i32> {
        let mut request = request.into_iter();
        let name = request.next().unwrap_or_default();
        let Some(&(tool, ref path)) = self.tools.iter().find(|(tool, _)| name == tool.name())
        else {
            return Err(io::Error::other(format!(
                "{}: not a tool",
                name.to_string_lossy()
            )));
        };
        let cwd = PathBuf::from(request.next().unwrap_or_default());
        let args: Vec<OsString> = request.collect();
        let cwd = cwd.canonicalize().map_err(|e| at(&cwd, e))?;
        if !cwd.starts_with(&self.dir) {
            return Err(io::Error::other(format!(
                "{}: {} runs only in {}",
                cwd.display(),
                tool.name(),
                self.dir.display()
            )));
        }
        let Some(args) = args
            .iter()
            .map(|arg| arg.to_str())
            .collect::<Option<Vec<&str>>>()
        else {
            return Err(io::Error::other(format!(
                "{}'s arguments must be UTF-8",
                tool.name()
            )));
        };
        let line = quoted(tool.name(), &args);
        let always = match tool.policy(&args) {
            Policy::Never(what) => {
                return Err(io::Error::other(format!("{what}: never run for it")));
            }
            Policy::AskEachTime => None,
            Policy::Ask(what) => Some(what),
        };
        if !self.allowed(tool.name(), &line, &cwd, always.as_deref())? {
            return Err(io::Error::other(format!(
                "{line}: not allowed; no one allowed it in time, or it was denied"
            )));
        }
        let [stdin, stdout, stderr] = stdio;
        let mut command = Command::new(path);
        command
            .args(&args)
            .current_dir(&cwd)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr);
        let finish = tool.prepare(&self.repo, &mut command)?;
        // `confine` ignores the terminal's signals while it waits, which the
        // tool gets as they are by default.
        unsafe {
            command.pre_exec(|| {
                for signal in crate::SIGNALS {
                    libc::signal(signal, libc::SIG_DFL);
                }
                Ok(())
            });
        }
        let status = command.status().map_err(|e| at(path, e));
        if let Err(e) = finish() {
            let _ = writeln!(err, "confine: {e}");
        }
        let status = status?;
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
    }

    /// Whether you allow `tool` running `line` in `cwd`: once, or, if
    /// `always` is given, always, which is kept in the file of what you
    /// allowed so.
    fn allowed(
        &self,
        tool: &str,
        line: &str,
        cwd: &Path,
        always: Option<&str>,
    ) -> io::Result<bool> {
        let _asking = self.asking.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(always) = always
            && allowed(&self.allowed)?.contains(always)
        {
            return Ok(true);
        }
        let question = format!(
            "The command confined to {} wants to run:\n\n{line}\n\nin {}",
            self.dir.display(),
            cwd.display()
        );
        Ok(match ask(&question, tool, line, &self.dir, cwd, always)? {
            Answer::Deny => false,
            Answer::Once => true,
            Answer::Always => {
                if let Some(always) = always {
                    remember(&self.allowed, always)?;
                }
                true
            }
        })
    }
}

/// What the file `path` lists as allowed always: its lines, but blank ones
/// and comments.
fn allowed(path: &Path) -> io::Result<HashSet<String>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(at(path, e)),
    };
    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// Adds `always` to the file `path` of what is allowed always.
fn remember(path: &Path, always: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| at(parent, e))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| at(path, e))?;
    writeln!(file, "{always}").map_err(|e| at(path, e))
}

/// Asks you whether to let `tool` run `line`, from the command confined to
/// `dir`, in `cwd`, with a dialog, or `question`, with the program
/// `CONFINE_ASK` names, offering to allow once, and, if `always` is given,
/// always so.
fn ask(
    question: &str,
    tool: &str,
    line: &str,
    dir: &Path,
    cwd: &Path,
    always: Option<&str>,
) -> io::Result<Answer> {
    let out = match env::var_os(ASK) {
        Some(program) => Command::new(&program).arg(question).output()?,
        // What the dialog shows is given as arguments, so it needs no
        // quoting.
        None => Command::new("/usr/bin/osascript")
            .args(["-l", "JavaScript", "-e", DIALOG, tool, line])
            .arg(dir)
            .arg(cwd)
            .arg(TIMEOUT.to_string())
            .args(always)
            .stderr(Stdio::null())
            .output()?,
    };
    Ok(match String::from_utf8_lossy(&out.stdout).trim() {
        "once" => Answer::Once,
        "always" if always.is_some() => Answer::Always,
        "always" => Answer::Once,
        _ => Answer::Deny,
    })
}

/// `name` and `args` as a shell would take them, with what is not plain
/// quoted, and control characters escaped, so that you see each argument as
/// it is.
fn quoted(name: &str, args: &[&str]) -> String {
    let mut line = String::from(name);
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

/// Whether this process is a shim: `confine` run under a tool's name by a
/// confined command.
pub fn is_shim() -> bool {
    env::var_os(SOCKET).is_some()
        && shim_name().is_some_and(|name| TOOLS.iter().any(|tool| tool.name() == name))
}

/// The name this process was run under.
fn shim_name() -> Option<OsString> {
    let arg0 = env::args_os().next()?;
    Path::new(&arg0).file_name().map(OsString::from)
}

/// Runs as a shim: has the broker run the tool, and exits as it did.
pub fn shim() -> ExitCode {
    let name = shim_name().unwrap_or_default();
    match request(&name) {
        Ok(status) => ExitCode::from(status as u8),
        Err(e) => {
            eprintln!("{}: confine: {e}", name.to_string_lossy());
            ExitCode::FAILURE
        }
    }
}

/// Sends the broker the standard streams, `name`, the working directory and
/// the arguments, and returns the exit status it sends back.
fn request(name: &OsString) -> io::Result<i32> {
    let mut stream = UnixStream::connect(env::var_os(SOCKET).unwrap_or_default())?;
    send_fds(&stream, &[0, 1, 2])?;
    let mut request = vec![name.clone(), env::current_dir()?.into_os_string()];
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
                    // So that a tool run for another request gets none of
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_what_is_not_plain() {
        assert_eq!(
            quoted("gh", &["pr", "create", "--title", "it's", "a\nb", ""]),
            r"gh pr create --title 'it'\''s' $'a\nb' ''"
        );
    }

    #[test]
    fn reads_what_is_allowed_always() {
        let dir = env::temp_dir().join(format!("confine-allowed-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("confine/allow");
        assert!(allowed(&path).unwrap().is_empty());
        remember(&path, "gh pr create").unwrap();
        fs::write(
            &path,
            fs::read_to_string(&path).unwrap() + "\n# x\n  gh pr view \n",
        )
        .unwrap();
        assert_eq!(
            allowed(&path).unwrap(),
            HashSet::from(["gh pr create".to_string(), "gh pr view".to_string()])
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
