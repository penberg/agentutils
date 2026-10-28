//! Linux: a command is confined with Landlock.

use std::{
    ffi::CString,
    fs, io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::Command,
    ptr,
};

// Access rights, from `<linux/landlock.h>`. Those after `MAKE_SYM` came in
// later versions of Landlock's ABI: `REFER` in 2 and `TRUNCATE` in 3.
const EXECUTE: u64 = 1 << 0;
const WRITE_FILE: u64 = 1 << 1;
const READ_FILE: u64 = 1 << 2;
const REMOVE_DIR: u64 = 1 << 4;
const REMOVE_FILE: u64 = 1 << 5;
const MAKE_CHAR: u64 = 1 << 6;
const MAKE_DIR: u64 = 1 << 7;
const MAKE_REG: u64 = 1 << 8;
const MAKE_SOCK: u64 = 1 << 9;
const MAKE_FIFO: u64 = 1 << 10;
const MAKE_BLOCK: u64 = 1 << 11;
const MAKE_SYM: u64 = 1 << 12;
const REFER: u64 = 1 << 13;
const TRUNCATE: u64 = 1 << 14;

/// The rights to read a file's contents or run it. Listing a directory is not
/// among them, so the command may list any directory, hidden ones included,
/// though it may not read what is in them.
const READ: u64 = EXECUTE | READ_FILE;

/// The rights to change files and directories.
const WRITE: u64 = WRITE_FILE
    | REMOVE_DIR
    | REMOVE_FILE
    | MAKE_CHAR
    | MAKE_DIR
    | MAKE_REG
    | MAKE_SOCK
    | MAKE_FIFO
    | MAKE_BLOCK
    | MAKE_SYM
    | REFER
    | TRUNCATE;

/// The rights that apply to a file that is not a directory.
const FILE: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE;

/// The version of Landlock's ABI with `REFER`, without which a file may not
/// be moved or linked to another directory, as git does.
const MIN_ABI: i64 = 2;

const CREATE_RULESET_VERSION: u32 = 1 << 0;
const RULE_PATH_BENEATH: u32 = 1;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// A command that runs `command` under Landlock: it may write only under
/// `writable` and to devices, and may not read or write anything under
/// `hidden` except `dir`, which it may read and write. Everything else, the
/// network included, is allowed.
///
/// Landlock only grants rights, to a file and everything under it, so the
/// command is granted the rights to what is around the hidden paths rather
/// than denied those to the hidden paths: what is in a directory above them
/// when the command starts. The paths must be canonical, as Landlock grants
/// rights to the file a path resolves to.
pub fn confine(
    command: &[String],
    dir: &Path,
    writable: &[PathBuf],
    hidden: &[PathBuf],
) -> io::Result<Command> {
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            ptr::null::<RulesetAttr>(),
            0,
            CREATE_RULESET_VERSION,
        )
    };
    if abi < MIN_ABI {
        return Err(io::Error::other(
            "confine needs Landlock, in Linux 5.19 or later, enabled",
        ));
    }
    let mut handled = READ | WRITE;
    if abi < 3 {
        handled &= !TRUNCATE;
    }
    let attr = RulesetAttr {
        handled_access_fs: handled,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr,
            size_of::<RulesetAttr>(),
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let ruleset = Ruleset {
        fd: unsafe { OwnedFd::from_raw_fd(fd as i32) },
        handled,
        hidden,
    };
    ruleset.allow_except_hidden(Path::new("/"), READ)?;
    ruleset.allow_except_hidden(Path::new("/dev"), WRITE)?;
    for path in writable {
        ruleset.allow_except_hidden(path, WRITE)?;
    }
    ruleset.allow(dir, READ | WRITE)?;
    let fd = ruleset.fd;
    let mut confined = Command::new(&command[0]);
    confined.args(&command[1..]);
    unsafe {
        confined.pre_exec(move || {
            // Landlock may confine a process without privileges only if it
            // can gain none, as by running a set-user-ID program.
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::syscall(libc::SYS_landlock_restrict_self, fd.as_raw_fd(), 0) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(confined)
}

/// A Landlock ruleset being made.
struct Ruleset<'a> {
    fd: OwnedFd,
    /// The rights the ruleset handles: those it denies unless granted.
    handled: u64,
    /// The paths no rights are granted under.
    hidden: &'a [PathBuf],
}

impl Ruleset<'_> {
    /// Grants `access` to `path` and everything under it except what is
    /// hidden: to `path` if nothing under it is, or else to each file in it,
    /// in the same way.
    fn allow_except_hidden(&self, path: &Path, access: u64) -> io::Result<()> {
        if self.hidden.iter().any(|hidden| path.starts_with(hidden)) {
            return Ok(());
        }
        if !self.hidden.iter().any(|hidden| hidden.starts_with(path)) {
            return self.allow(path, access);
        }
        for entry in fs::read_dir(path)? {
            self.allow_except_hidden(&entry?.path(), access)?;
        }
        Ok(())
    }

    /// Grants `access` to `path` and everything under it, or only those rights
    /// that apply if it is not a directory. A symbolic link is not granted
    /// anything, as Landlock checks what it points to, nor is a file that
    /// cannot be opened, such as one gone since its directory was listed.
    fn allow(&self, path: &Path, access: u64) -> io::Result<()> {
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return Ok(());
        };
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Ok(());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let access = match stat.st_mode & libc::S_IFMT {
            libc::S_IFDIR => access,
            libc::S_IFLNK => return Ok(()),
            _ => access & FILE,
        } & self.handled;
        if access == 0 {
            return Ok(());
        }
        let attr = PathBeneathAttr {
            allowed_access: access,
            parent_fd: fd.as_raw_fd(),
        };
        let added = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                self.fd.as_raw_fd(),
                RULE_PATH_BENEATH,
                &attr,
                0,
            )
        };
        if added != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
