use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    process::Child,
};
pub(super) const CAPACITY: usize = 256;
pub(super) struct Object(OwnedFd);
pub(super) struct Signal(OwnedFd);
impl Object {
    pub fn new(child: &Child) -> Option<Self> {
        // pidfd_open is non-reaping and CLOEXEC. ENOSYS/EPERM/EMFILE etc.
        // retain the existing bounded fallback for this particular run.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0u32) };
        (fd >= 0).then(|| Self(unsafe { OwnedFd::from_raw_fd(fd as i32) }))
    }
}
impl Signal {
    pub fn new() -> Option<Self> {
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        (fd >= 0).then(|| Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }
    pub fn notify(&self) {
        let value = 1u64;
        loop {
            let result =
                unsafe { libc::write(self.0.as_raw_fd(), (&value as *const u64).cast(), 8) };
            if result >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
        // EAGAIN means a wake is already pending (counter full).
    }
    pub fn wait(&self, objects: &[&Object]) -> io::Result<Vec<usize>> {
        let mut fds = Vec::with_capacity(objects.len() + 1);
        fds.push(libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        fds.extend(objects.iter().map(|object| libc::pollfd {
            fd: object.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }));
        loop {
            if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } >= 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        if fds
            .iter()
            .any(|fd| fd.revents & (libc::POLLNVAL | libc::POLLERR) != 0)
        {
            return Err(io::Error::other("invalid native exit wait descriptor"));
        }
        if fds[0].revents != 0 {
            let mut value = 0u64;
            unsafe {
                libc::read(self.0.as_raw_fd(), (&mut value as *mut u64).cast(), 8);
            }
        }
        Ok(fds[1..]
            .iter()
            .enumerate()
            .filter_map(|(index, fd)| {
                (fd.revents & (libc::POLLIN | libc::POLLHUP) != 0).then_some(index)
            })
            .collect())
    }
}
