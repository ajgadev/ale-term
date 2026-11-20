//! Finding the working directory of the process running in a PTY.
//!
//! Used so that a new tab or split opens in the same folder as the pane it
//! was created from.

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
