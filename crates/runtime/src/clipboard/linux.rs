use super::{ClipboardError, Zeroizing};
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;
mod wayland;
mod x11;

type Result<T> = std::result::Result<T, ClipboardError>;
const TIMEOUT: Duration = Duration::from_secs(3);

fn access(_: impl std::fmt::Debug) -> ClipboardError {
    ClipboardError::Access
}

fn check(deadline: Instant, cancelled: &dyn Fn() -> bool) -> Result<()> {
    if cancelled() {
        Err(ClipboardError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(ClipboardError::Access)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Session {
    X11,
    Wayland,
}

fn session() -> Result<Session> {
    let present = |key| std::env::var_os(key).is_some_and(|value| !value.is_empty());
    if std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland")
        || present("WAYLAND_DISPLAY")
        || present("WAYLAND_SOCKET")
    {
        Ok(Session::Wayland)
    } else if present("DISPLAY") {
        Ok(Session::X11)
    } else {
        Err(ClipboardError::UnsupportedBackend)
    }
}

pub(super) fn available() -> bool {
    use std::sync::Mutex;
    // Settings may query many scopes at once. Probe only the display protocol, never the selection.
    static PROBE: Mutex<Option<(Session, Instant, bool)>> = Mutex::new(None);
    let Ok(session) = session() else { return false };
    let mut probe = PROBE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((previous, when, available)) = *probe {
        if previous == session && when.elapsed() < Duration::from_secs(1) {
            return available;
        }
    }
    let available = match session {
        Session::X11 => x11::available(),
        Session::Wayland => wayland::available(),
    };
    *probe = Some((session, Instant::now(), available));
    available
}

pub(super) fn read(cancelled: &dyn Fn() -> bool) -> Result<Zeroizing<String>> {
    if cancelled() {
        return Err(ClipboardError::Cancelled);
    }
    match session()? {
        Session::X11 => x11::read(cancelled),
        Session::Wayland => wayland::read(cancelled),
    }
}

pub(super) fn write(text: &str) -> Result<()> {
    match session()? {
        Session::X11 => x11::write(text),
        Session::Wayland => wayland::write(text),
    }
}

fn utf8(mut bytes: Zeroizing<Vec<u8>>) -> Result<Zeroizing<String>> {
    use zeroize::Zeroize;
    String::from_utf8(std::mem::take(&mut *bytes))
        .map(Zeroizing::new)
        .map_err(|error| {
            error.into_bytes().zeroize();
            ClipboardError::InvalidEncoding
        })
}
