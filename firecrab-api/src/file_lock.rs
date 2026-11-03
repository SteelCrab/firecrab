//! Advisory lock lifetime is independent of unrelated forked descriptor copies.

use std::{fs::File, ops::Deref};

/// Unlock before closing, so a concurrently forked child cannot extend the lease.
pub(crate) struct FileLock(
    /// The already locked descriptor; dropping the guard never reopens its path.
    pub(crate) File,
);

impl Deref for FileLock {
    type Target = File;

    fn deref(&self) -> &File {
        &self.0
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn iac_file_lock_drop_releases_the_lease_while_a_forked_copy_is_still_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lease");
        let file = File::create(&path).unwrap();
        file.try_lock().unwrap();
        let guard = FileLock(file);
        let (input, output) = std::os::unix::net::UnixStream::pair().unwrap();
        let input_fd = input.as_raw_fd();
        // SAFETY: the child calls only async-signal-safe read/_exit before exiting.
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            let mut byte = 0u8;
            // SAFETY: the inherited socket and one-byte stack buffer remain valid.
            unsafe {
                libc::read(input_fd, (&mut byte as *mut u8).cast(), 1);
                libc::_exit(0);
            }
        }
        drop(guard);
        let next = File::open(path).unwrap();
        let acquired = next.try_lock();
        // Always release/reap the child before asserting the parent's result.
        use std::io::Write;
        (&output).write_all(&[1]).unwrap();
        // SAFETY: this is our known child; no status pointer is requested.
        assert_eq!(
            unsafe { libc::waitpid(child, std::ptr::null_mut(), 0) },
            child
        );
        acquired.expect("a forked descriptor must not extend an ended publication lease");
    }
}
