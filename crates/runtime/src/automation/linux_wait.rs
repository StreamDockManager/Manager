//! Wait on X11 readability and configuration/shutdown, not a 50 ms pump.
use super::ObservationSignal;
use std::{
    io::{Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    sync::Arc,
    time::Duration,
};
pub(super) struct NativeWait {
    read: UnixStream,
    signal: Arc<ObservationSignal>,
}
impl NativeWait {
    pub(super) fn new(signal: Arc<ObservationSignal>) -> Result<Self, String> {
        let (read, write) = UnixStream::pair().map_err(|e| e.to_string())?;
        read.set_nonblocking(true).map_err(|e| e.to_string())?;
        write.set_nonblocking(true).map_err(|e| e.to_string())?;
        signal.native(Some(Arc::new(move || {
            let _ = (&write).write(&[1]);
        })));
        Ok(Self { read, signal })
    }
    pub(super) fn wait(&self, stream: &impl AsFd, timeout: Duration) -> Result<(), String> {
        use rustix::event::{poll, PollFd, PollFlags, Timespec};
        let mut fds = [
            PollFd::new(stream, PollFlags::IN),
            PollFd::new(&self.read, PollFlags::IN),
        ];
        let timeout = Timespec::try_from(timeout).map_err(|e| e.to_string())?;
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        }
        let mut buffer = [0; 128];
        while (&self.read).read(&mut buffer).is_ok_and(|size| size > 0) {}
        Ok(())
    }
}
impl Drop for NativeWait {
    fn drop(&mut self) {
        self.signal.native(None);
    }
}
