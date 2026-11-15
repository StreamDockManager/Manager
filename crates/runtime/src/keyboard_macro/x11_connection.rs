//! Private local-X11 connections with a deadline on every blocking I/O wait
//! No TCP/SSH display, XWayland fallback, system setting change, or global grab
use rustix::{
    event::{poll, PollFd, PollFlags, Timespec},
    net::{self, AddressFamily, SocketAddrUnix, SocketType},
};
use std::{
    io,
    os::unix::net::UnixStream,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use x11rb::{
    rust_connection::{DefaultStream, PollMode, RustConnection, Stream},
    utils::RawFdContainer,
};

pub(crate) type Connection = RustConnection<BoundedStream>;
pub(crate) struct BoundedStream {
    inner: DefaultStream,
    epoch: Instant,
    deadline_ms: AtomicU64,
}
impl BoundedStream {
    /// XRes returns PIDs in the server's namespace. Callers must verify that
    /// namespace before interpreting them as local process identities
    #[cfg(target_os = "linux")]
    // Read the local X11 socket peer process identity
    pub(crate) fn peer_pid(&self) -> Result<u32, String> {
        let credentials = net::sockopt::socket_peercred(&self.inner)
            .map_err(|_| "The local X11 server identity cannot be verified")?;
        u32::try_from(credentials.pid.as_raw_nonzero().get())
            .map_err(|_| "The local X11 server identity is invalid".into())
    }
    // Arm the X11 connection operation for its next event
    pub fn arm(&self, duration: Duration) {
        self.deadline_ms.store(
            self.epoch
                .elapsed()
                .saturating_add(duration)
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
            Ordering::Release,
        );
    }
    // Check the current X11 connection conditions
    fn check(&self) -> io::Result<()> {
        if self.epoch.elapsed().as_millis() >= u128::from(self.deadline_ms.load(Ordering::Acquire))
        {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "X11 keyboard connection timed out",
            ))
        } else {
            Ok(())
        }
    }
}
impl Stream for BoundedStream {
    // Poll X11 connection for pending work
    fn poll(&self, mode: PollMode) -> io::Result<()> {
        loop {
            self.check()?;
            let flags = match mode {
                PollMode::Readable => PollFlags::IN,
                PollMode::Writable => PollFlags::OUT,
                PollMode::ReadAndWritable => PollFlags::IN | PollFlags::OUT,
            };
            let mut descriptors = [PollFd::new(&self.inner, flags)];
            match poll(
                &mut descriptors,
                Some(&Timespec {
                    tv_sec: 0,
                    tv_nsec: 5_000_000,
                }),
            ) {
                Ok(0) => continue,
                Ok(_)
                    if descriptors[0]
                        .revents()
                        .intersects(PollFlags::ERR | PollFlags::HUP | PollFlags::NVAL) =>
                {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "X11 keyboard connection closed",
                    ));
                }
                Ok(_) => return Ok(()),
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }
    // Read the requested X11 connection data
    fn read(&self, bytes: &mut [u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.check()?;
        self.inner.read(bytes, fds)
    }
    // Write the supplied X11 connection data
    fn write(&self, bytes: &[u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.check()?;
        self.inner.write(bytes, fds)
    }
}

// Validate that the display refers to a supported local X11 connection
fn local_display(value: &str) -> Result<(u16, usize), String> {
    let value = value.strip_prefix("unix/").unwrap_or(value);
    let value = value
        .strip_prefix(':')
        .ok_or("Only a local X11 display is supported")?;
    let (display, screen) = value.split_once('.').unwrap_or((value, "0"));
    if display.is_empty()
        || screen.is_empty()
        || !display.bytes().all(|b| b.is_ascii_digit())
        || !screen.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("The local X11 display is invalid".into());
    }
    Ok((
        display
            .parse()
            .map_err(|_| "The local X11 display is invalid")?,
        screen
            .parse()
            .map_err(|_| "The local X11 screen is invalid")?,
    ))
}

// Connect socket
fn connect_socket(address: &SocketAddrUnix) -> io::Result<UnixStream> {
    #[cfg(target_os = "linux")]
    let fd = net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        net::SocketFlags::NONBLOCK | net::SocketFlags::CLOEXEC,
        None,
    )?;
    #[cfg(not(target_os = "linux"))]
    let fd = {
        // The production adapter is Linux-only. This portable branch lets its
        // protocol code compile in non-injecting host-side harnesses
        let fd = net::socket(AddressFamily::UNIX, SocketType::STREAM, None)?;
        rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)?;
        rustix::io::ioctl_fionbio(&fd, true)?;
        fd
    };
    match net::connect(&fd, address) {
        Ok(()) => (),
        Err(rustix::io::Errno::INPROGRESS | rustix::io::Errno::AGAIN) => {
            let started = Instant::now();
            loop {
                if started.elapsed() >= Duration::from_millis(250) {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Local X11 connection timed out",
                    ));
                }
                let mut descriptors = [PollFd::new(&fd, PollFlags::OUT)];
                match poll(
                    &mut descriptors,
                    Some(&Timespec {
                        tv_sec: 0,
                        tv_nsec: 5_000_000,
                    }),
                ) {
                    Ok(0) => (),
                    Ok(_) => {
                        net::sockopt::socket_error(&fd)??;
                        break;
                    }
                    Err(rustix::io::Errno::INTR) => (),
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(UnixStream::from(fd))
}

// Establish the X11 connection connection
pub(crate) fn connect() -> Result<(Connection, usize), String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland")
    {
        return Err("X11 keyboard sessions never fall back to XWayland".into());
    }
    let (display, screen) =
        local_display(&std::env::var("DISPLAY").map_err(|_| "No local X11 display")?)?;
    let path = format!("/tmp/.X11-unix/X{display}");
    let address = SocketAddrUnix::new(path.as_str()).map_err(|_| "Invalid X11 socket")?;
    let connection = connect_socket(&address);
    #[cfg(target_os = "linux")]
    let connection = connection.or_else(|_| {
        let address = SocketAddrUnix::new_abstract_name(path.as_bytes())?;
        connect_socket(&address)
    });
    let (inner, (family, address)) = DefaultStream::from_unix_stream(
        connection.map_err(|_| "Local X11 connection is unavailable")?,
    )
    .map_err(|_| "Local X11 connection is unavailable")?;
    let (name, data) = x11rb_protocol::xauth::get_auth(family, &address, display)
        .map_err(|_| "X11 session authorization is unavailable")?
        .unwrap_or_default();
    let stream = BoundedStream {
        inner,
        epoch: Instant::now(),
        deadline_ms: AtomicU64::new(2_000),
    };
    let connection = RustConnection::connect_to_stream_with_auth_info(stream, screen, name, data)
        .map_err(|_| "X11 session authorization or setup failed")?;
    Ok((connection, screen))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that only local display names are admitted
    fn only_local_display_names_are_admitted() {
        assert_eq!(local_display(":0"), Ok((0, 0)));
        assert_eq!(local_display("unix/:12.3"), Ok((12, 3)));
        for value in [
            "host:0",
            "localhost:10",
            ":-1",
            ":0.extra",
            ":0.1.2",
            "/tmp/X0",
            ":",
        ] {
            assert!(local_display(value).is_err());
        }
    }
}
