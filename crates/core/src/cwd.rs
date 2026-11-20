//! Asking the operating system about a PTY and the processes in it: the
//! foreground program's working directory (so a new tab or split opens in
//! the same folder), and the terminal's device number.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

/// The process group currently in the foreground of the PTY: the shell while
/// it sits at a prompt, or whatever command it is running.
pub fn foreground_pid(pty: &File) -> Option<i32> {
    // SAFETY: `tcgetpgrp` only reads from a file descriptor we own and that
    // stays open for the duration of the call.
    let pid = unsafe { libc::tcgetpgrp(pty.as_raw_fd()) };
    (pid > 0).then_some(pid)
}

#[cfg(target_os = "macos")]
pub fn of_pid(pid: i32) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::mem::{MaybeUninit, size_of};

    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: we pass a buffer of exactly the size we declare; the kernel
    // fills it and returns the number of bytes written.
    let written = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, info.as_mut_ptr().cast(), size)
    };
    if written != size {
        return None;
    }
    // SAFETY: the call succeeded, so the struct is initialized and the path
    // is a NUL-terminated C string inside it.
    let path = unsafe {
        let info = info.assume_init_ref();
        CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()).to_owned()
    };
    let path = PathBuf::from(path.to_str().ok()?);
    (!path.as_os_str().is_empty()).then_some(path)
}

#[cfg(not(target_os = "macos"))]
pub fn of_pid(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Device number of the terminal on the shell's side of the PTY.
///
/// Must be called from one thread at a time (`ptsname` returns a pointer
/// to shared storage); sessions are only started from the main thread.
pub fn tty_device(pty: &File) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: `ptsname` returns NULL or a NUL-terminated path valid until
    // the next call; we copy it out immediately.
    let path = unsafe {
        let name = libc::ptsname(pty.as_raw_fd());
        if name.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(name).to_owned()
    };
    let metadata = std::fs::metadata(path.to_str().ok()?).ok()?;
    Some(metadata.rdev())
}

/// Device number of the calling process's controlling terminal, if it has
/// one. The counterpart of [`tty_device`].
#[cfg(target_os = "macos")]
pub fn controlling_tty() -> Option<u64> {
    use std::mem::{MaybeUninit, size_of};
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer is exactly the size we declare.
    let written = unsafe {
        libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size)
    };
    if written != size {
        return None;
    }
    // SAFETY: the call succeeded, so the struct is initialized.
    let device = unsafe { info.assume_init_ref().e_tdev };
    (device != u32::MAX && device != 0).then_some(device as u64)
}

#[cfg(not(target_os = "macos"))]
pub fn controlling_tty() -> Option<u64> {
    None
}
