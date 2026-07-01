//! Temporarily redirect the process's stderr (fd 2) to `/dev/null`, restoring
//! it on drop.
//!
//! Native plugin/library code (ALSA/JACK device probing, CLAP preset
//! scanners, VST3 factories, …) writes diagnostics straight to the stderr file
//! descriptor, bypassing Rust's `log` framework entirely. During `enumerate`
//! that just clutters the output; during the TUI it corrupts the alternate
//! screen. This guard silences it at the fd level for the duration it's held.

/// Redirect stderr to `/dev/null` while held; restores the original stderr when
/// dropped. `new()` returns `None` (leaving stderr untouched) if the redirect
/// can't be set up, or on non-Unix platforms.
#[cfg(unix)]
pub struct StderrSilencer {
    saved_fd: i32,
}

#[cfg(unix)]
impl StderrSilencer {
    pub fn new() -> Option<Self> {
        use std::os::unix::io::AsRawFd;
        let devnull = std::fs::File::open("/dev/null").ok()?;
        let stderr_fd = std::io::stderr().as_raw_fd();
        // SAFETY: dup/dup2 on the process stderr fd; the saved fd is restored
        // (and the duplicate closed) in Drop.
        let saved = unsafe { libc::dup(stderr_fd) };
        if saved < 0 {
            return None;
        }
        unsafe {
            libc::dup2(devnull.as_raw_fd(), stderr_fd);
        }
        Some(Self { saved_fd: saved })
    }
}

#[cfg(unix)]
impl Drop for StderrSilencer {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        let stderr_fd = std::io::stderr().as_raw_fd();
        // SAFETY: restore the original stderr and close the duplicate.
        unsafe {
            libc::dup2(self.saved_fd, stderr_fd);
            libc::close(self.saved_fd);
        }
    }
}

#[cfg(not(unix))]
pub struct StderrSilencer;

#[cfg(not(unix))]
impl StderrSilencer {
    pub fn new() -> Option<Self> {
        None
    }
}
