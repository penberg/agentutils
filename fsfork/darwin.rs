//! macOS: a directory is cloned with `copyfile`, which clones each file in
//! it.
//!
//! `clonefile` can clone a whole directory in one call, but its man page
//! says "Cloning directories with these functions is strongly discouraged.
//! Use copyfile(3) to clone directories instead.", as cloning a directory
//! holds off every change to it until the clone is done, which on a large
//! directory, or one the system uses, can stall the system, and in the worst
//! case panic the kernel:
//!
//! - <https://developer.apple.com/forums/thread/784446>
//! - <https://developer.apple.com/forums/thread/818372>
//!
//! So the directory is cloned file by file, and something written to it
//! meanwhile may be in the clone or not, each file by itself.

use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fs, io,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
    },
    path::{Path, PathBuf},
    process,
    time::SystemTime,
};

use libc::{c_char, c_int, c_void, copyfile_state_t};

/// Clones the directory tree at `from` to `to`, which must not exist yet.
/// The clone shares its blocks with the original until either is written.
///
/// The clone is made next to `to` and moved there when done, so that `to`
/// is never there half made, and fails if `to` exists, as `copyfile` would
/// copy into it instead.
pub fn clone(from: &Path, to: &Path) -> io::Result<()> {
    same_volume(from, to)?;
    let part = part_of(to);
    let made = copy(from, &part, true).and_then(|()| rename_new(&part, to));
    if made.is_err() {
        let _ = fs::remove_dir_all(&part);
    }
    made
}

/// Clones `names`, entries of the directory `from`, into the directory `to`.
///
/// FIFOs are made anew, and sockets and devices left out, as nothing is
/// listening on or behind a clone of them, and an entry gone since it was
/// listed is left out.
pub fn clone_entries(from: &Path, names: &[OsString], to: &Path) -> io::Result<()> {
    for name in names {
        let (from, to) = (from.join(name), to.join(name));
        let meta = match from.symlink_metadata() {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if !special(&meta, &to)? {
            copy(&from, &to, meta.is_dir())?;
        }
    }
    Ok(())
}

/// Fails as `clonefile` would if `to` is on another volume than `from`,
/// where `copyfile` would copy every file rather than clone it.
fn same_volume(from: &Path, to: &Path) -> io::Result<()> {
    let parent = to.parent().unwrap_or(Path::new("."));
    if from.metadata()?.dev() != parent.metadata()?.dev() {
        return Err(io::Error::from_raw_os_error(libc::EXDEV));
    }
    Ok(())
}

/// Where `to` is made before it is moved there: hidden next to it, and
/// named after this process, so that two that make the same `to` at once do
/// not make it in the same place.
fn part_of(to: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(to.file_name().unwrap_or_default());
    name.push(format!(".{}.part", process::id()));
    to.with_file_name(name)
}

/// Moves `from` to `to`, failing if `to` exists.
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (cstring(from)?, cstring(to)?);
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Makes a FIFO at `to` like the one `meta` is of, and leaves sockets and
/// devices out. Says whether it was one of them.
fn special(meta: &fs::Metadata, to: &Path) -> io::Result<bool> {
    let kind = meta.file_type();
    if kind.is_fifo() {
        let to = cstring(to)?;
        if unsafe { libc::mkfifo(to.as_ptr(), meta.mode() as libc::mode_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(kind.is_fifo() || kind.is_socket() || kind.is_block_device() || kind.is_char_device())
}

/// What a copy is at, for its callback.
#[derive(Default)]
struct Copying {
    /// Why it stopped, if it did.
    failed: Option<io::Error>,
    /// The directories made, and when those they were made from were
    /// modified, which they are set to once all in them is made.
    dirs: Vec<(PathBuf, SystemTime)>,
}

/// Clones `from` to `to`, which must not exist yet, and if `from` is a
/// directory, all in it. A symbolic link is cloned itself, but only given
/// as a directory's entry if copying recursively, as `copyfile` follows it
/// otherwise.
fn copy(from: &Path, to: &Path, dir: bool) -> io::Result<()> {
    let (src, dst) = (cstring(from)?, cstring(to)?);
    let mut copying = Copying::default();
    let state = unsafe { libc::copyfile_state_alloc() };
    if state.is_null() {
        return Err(io::Error::last_os_error());
    }
    let callback: extern "C" fn(
        c_int,
        c_int,
        copyfile_state_t,
        *const c_char,
        *const c_char,
        *mut c_void,
    ) -> c_int = status;
    let copied = unsafe {
        libc::copyfile_state_set(
            state,
            libc::COPYFILE_STATE_STATUS_CB as u32,
            callback as *const c_void,
        );
        libc::copyfile_state_set(
            state,
            libc::COPYFILE_STATE_STATUS_CTX as u32,
            &mut copying as *mut Copying as *const c_void,
        );
        let copied = libc::copyfile(
            src.as_ptr(),
            dst.as_ptr(),
            state,
            if dir {
                libc::COPYFILE_CLONE | libc::COPYFILE_RECURSIVE
            } else {
                libc::COPYFILE_CLONE
            },
        );
        let e = io::Error::last_os_error();
        libc::copyfile_state_free(state);
        if copied == 0 { Ok(()) } else { Err(e) }
    };
    if let Some(e) = copying.failed {
        return Err(e);
    }
    copied?;
    // Making a symbolic link in a directory changes its modification time,
    // and `copyfile` makes them after it sets the directory's.
    for (dir, modified) in copying.dirs.iter().rev() {
        match fs::File::open(dir).and_then(|dir| dir.set_modified(*modified)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// `copyfile`'s callback, for each file and directory it copies: makes
/// FIFOs anew, leaves out sockets, devices and what is gone since it was
/// listed, notes the directories made, and stops at anything else that
/// fails.
extern "C" fn status(
    what: c_int,
    stage: c_int,
    _state: copyfile_state_t,
    src: *const c_char,
    dst: *const c_char,
    ctx: *mut c_void,
) -> c_int {
    let e = io::Error::last_os_error();
    let copying = unsafe { &mut *(ctx as *mut Copying) };
    let path = |p: *const c_char| -> PathBuf {
        if p.is_null() {
            return PathBuf::new();
        }
        PathBuf::from(OsStr::from_bytes(unsafe { CStr::from_ptr(p) }.to_bytes()))
    };
    if stage == libc::COPYFILE_ERR {
        if e.kind() == io::ErrorKind::NotFound {
            return libc::COPYFILE_SKIP;
        }
        copying.failed = Some(io::Error::new(
            e.kind(),
            format!("{}: {e}", path(src).display()),
        ));
        return libc::COPYFILE_QUIT;
    }
    if stage != libc::COPYFILE_START {
        return libc::COPYFILE_CONTINUE;
    }
    let (src, dst) = (path(src), path(dst));
    let meta = match src.symlink_metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return libc::COPYFILE_SKIP,
        Err(e) => {
            copying.failed = Some(io::Error::new(e.kind(), format!("{}: {e}", src.display())));
            return libc::COPYFILE_QUIT;
        }
    };
    if what == libc::COPYFILE_RECURSE_DIR {
        if let Ok(modified) = meta.modified() {
            copying.dirs.push((dst, modified));
        }
    } else if what == libc::COPYFILE_RECURSE_FILE {
        match special(&meta, &dst) {
            Ok(true) => return libc::COPYFILE_SKIP,
            Ok(false) => {}
            Err(e) => {
                copying.failed = Some(io::Error::new(e.kind(), format!("{}: {e}", dst.display())));
                return libc::COPYFILE_QUIT;
            }
        }
    }
    libc::COPYFILE_CONTINUE
}

fn cstring(path: &Path) -> io::Result<CString> {
    Ok(CString::new(path.as_os_str().as_bytes())?)
}
