//! Writes a project's layer file under `.canvas/` at a git root without ever
//! following a symlink inside the repo (ADR-0005). The repo's contents are
//! not the person's to vouch for, since a clone can commit any symlink, so
//! every step goes through one descriptor for `.canvas` opened with
//! `O_NOFOLLOW`: the temp file is created in it exclusively (`O_EXCL`, which
//! refuses an existing name, link or not) under a random name, then renamed
//! into place there. Swapping `.canvas` for a link after the open changes
//! nothing this writes.

use std::ffi::CString;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use canvas_core::instructions::PROJECT_DIR;

fn c(name: &str) -> std::io::Result<CString> {
    CString::new(name).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
}

fn check(ret: libc::c_int) -> std::io::Result<libc::c_int> {
    if ret < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// Opens `name` in `dir` as a directory, refusing a symlink (`ELOOP`).
fn open_dir(dir: libc::c_int, name: &CString) -> std::io::Result<OwnedFd> {
    // SAFETY: `name` is a valid C string; the returned descriptor is owned.
    let fd = check(unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })?;
    // SAFETY: `fd` was just opened and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Opens `root` itself, which canvasd recorded from a session's working
/// directory.
fn open_root(root: &Path) -> std::io::Result<OwnedFd> {
    let path = CString::new(root.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    open_dir(libc::AT_FDCWD, &path)
}

/// Writes `text` to `.canvas/<file_name>` at `root`, creating `.canvas/` when
/// absent. Returns whether it created the folder.
pub fn write(root: &Path, file_name: &str, text: &[u8]) -> std::io::Result<bool> {
    let root_fd = open_root(root)?;
    let folder = c(PROJECT_DIR)?;
    // SAFETY: valid descriptor and C string. mkdir never follows a symlink.
    let created = match check(unsafe { libc::mkdirat(root_fd.as_raw_fd(), folder.as_ptr(), 0o755) })
    {
        Ok(_) => true,
        Err(e) if e.raw_os_error() == Some(libc::EEXIST) => false,
        Err(e) => return Err(e),
    };
    let dir = open_dir(root_fd.as_raw_fd(), &folder)?;
    let tmp = c(&format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ))?;
    // SAFETY: valid descriptor and C string; the descriptor is owned below.
    let fd = check(unsafe {
        libc::openat(
            dir.as_raw_fd(),
            tmp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o644 as libc::c_uint,
        )
    })?;
    // SAFETY: `fd` was just opened and nothing else owns it.
    let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    let target = c(file_name)?;
    let written = file.write_all(text).and_then(|()| {
        // SAFETY: valid descriptor and C strings. rename replaces a link at
        // the target rather than following it.
        check(unsafe {
            libc::renameat(
                dir.as_raw_fd(),
                tmp.as_ptr(),
                dir.as_raw_fd(),
                target.as_ptr(),
            )
        })
        .map(drop)
    });
    if written.is_err() {
        // SAFETY: valid descriptor and C string.
        unsafe { libc::unlinkat(dir.as_raw_fd(), tmp.as_ptr(), 0) };
    }
    written.map(|()| created)
}

/// Removes `.canvas/<file_name>` at `root`; a missing file or folder is
/// already removed.
pub fn remove(root: &Path, file_name: &str) -> std::io::Result<()> {
    let root_fd = open_root(root)?;
    let dir = match open_dir(root_fd.as_raw_fd(), &c(PROJECT_DIR)?) {
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(()),
        other => other?,
    };
    let target = c(file_name)?;
    // SAFETY: valid descriptor and C string. unlink never follows a symlink.
    match check(unsafe { libc::unlinkat(dir.as_raw_fd(), target.as_ptr(), 0) }) {
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(()),
        other => other.map(drop),
    }
}
