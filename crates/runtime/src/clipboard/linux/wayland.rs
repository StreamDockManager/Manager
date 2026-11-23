//! Use the compositor's regular selection. Each read binds its own one-shot offer.
use super::{access, check, utf8, ClipboardError, Result, Zeroizing, TIMEOUT};
use crate::clipboard::MAX_TEXT_BYTES;
use ext::{
    ext_data_control_manager_v1::ExtDataControlManagerV1 as ExtManager,
    ext_data_control_offer_v1::ExtDataControlOfferV1 as ExtOffer,
};
use rustix::{
    event::{poll, PollFd, PollFlags, Timespec},
    fs::{fcntl_getfl, fcntl_setfl, OFlags},
    pipe::{pipe_with, PipeFlags},
};
use std::{
    fs::File,
    io::{ErrorKind, Read, Write},
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    sync::{mpsc, Mutex},
    thread,
    time::Instant,
};
use wayland_client::{
    delegate_noop, event_created_child,
    globals::{registry_queue_init, GlobalList, GlobalListContents},
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::ext::data_control::v1::client as ext;
use wayland_protocols_wlr::data_control::v1::client as wlr;
use wlr::{
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1 as WlrManager,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1 as WlrOffer,
};

const MIME: &str = "text/plain;charset=utf-8";
const TICK: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 25_000_000,
};
type Formats = Mutex<Option<(u8, String)>>;

fn rank(mime: &str) -> Option<u8> {
    if mime.len() > 128 {
        return None;
    }
    if let Some((ty, charset)) = mime.split_once(';') {
        if ty.trim().eq_ignore_ascii_case("text/plain")
            && charset.trim().eq_ignore_ascii_case("charset=utf-8")
        {
            return Some(0);
        }
    }
    if mime == "UTF8_STRING" {
        return Some(1);
    }
    mime.eq_ignore_ascii_case("text/plain").then_some(2)
}

enum Offer {
    Ext(ExtOffer),
    Wlr(WlrOffer),
}
impl Offer {
    fn receive(&self, fd: BorrowedFd<'_>) -> Result<()> {
        let formats = match self {
            Self::Ext(o) => o.data::<Formats>(),
            Self::Wlr(o) => o.data::<Formats>(),
        };
        let formats = formats
            .ok_or(ClipboardError::Access)?
            .lock()
            .map_err(access)?;
        let (_, mime) = formats.as_ref().ok_or(ClipboardError::UnsupportedFormat)?;
        match self {
            Self::Ext(o) => o.receive(mime.clone(), fd),
            Self::Wlr(o) => o.receive(mime.clone(), fd),
        }
        Ok(())
    }
}
impl Drop for Offer {
    fn drop(&mut self) {
        match self {
            Self::Ext(o) => o.destroy(),
            Self::Wlr(o) => o.destroy(),
        }
    }
}

#[derive(Default)]
struct State {
    selection: Option<Offer>,
    generation: u64,
    finished: bool,
    seat_id: Option<u32>,
    published: Option<Zeroizing<String>>,
    pastes: Vec<Paste>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_registry::Event::GlobalRemove { name } if state.seat_id == Some(name))
        {
            state.finished = true;
        }
    }
}
type SeatName = Mutex<Option<String>>;
impl Dispatch<wl_seat::WlSeat, SeatName> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        label: &SeatName,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Name { name } = event {
            *label.lock().unwrap_or_else(|e| e.into_inner()) = Some(name);
        }
    }
}

// The standardized and wlroots protocols share the selection lifecycle and differ in their object types.
macro_rules! data_control {
    ($api:ident, $manager:ident, $device:ident::$dev:ident, $offer:ident::$off:ident, $source:ident::$src:ident, $variant:ident) => {
        delegate_noop!(State: ignore $manager);
        impl Dispatch<$api::$device::$dev, ()> for State {
            fn event(state: &mut Self, _: &$api::$device::$dev, event: $api::$device::Event,
                _: &(), _: &Connection, _: &QueueHandle<Self>) {
                use $api::$device::Event;
                match event {
                    Event::Selection { id } => {
                        state.selection = id.map(Offer::$variant);
                        state.generation += 1;
                    }
                    Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
                    Event::Finished => state.finished = true,
                    _ => {}
                }
            }
            event_created_child!(State, $api::$device::$dev, [
                $api::$device::EVT_DATA_OFFER_OPCODE => ($api::$offer::$off, Formats::default())
            ]);
        }
        impl Dispatch<$api::$offer::$off, Formats> for State {
            fn event(_: &mut Self, _: &$api::$offer::$off, event: $api::$offer::Event,
                formats: &Formats, _: &Connection, _: &QueueHandle<Self>) {
                if let $api::$offer::Event::Offer { mime_type } = event {
                    if let Some(priority) = rank(&mime_type) {
                        let mut best = formats.lock().unwrap_or_else(|e| e.into_inner());
                        if best.as_ref().is_none_or(|(p, _)| priority < *p) {
                            *best = Some((priority, mime_type));
                        }
                    }
                }
            }
        }
        impl Dispatch<$api::$source::$src, ()> for State {
            fn event(state: &mut Self, _: &$api::$source::$src, event: $api::$source::Event,
                _: &(), _: &Connection, _: &QueueHandle<Self>) {
                use $api::$source::Event;
                match event {
                    Event::Cancelled => state.finished = true,
                    Event::Send { mime_type, fd }
                        if !state.finished
                            && (mime_type == MIME || mime_type == "text/plain")
                            && state.published.is_some()
                            && state.pastes.len() < 8
                            && nonblocking(&fd).is_ok() =>
                    {
                        state.pastes.push(Paste { pipe: File::from(fd), sent: 0, deadline: Instant::now() + TIMEOUT });
                    }
                    _ => {}
                }
            }
        }
    };
}
data_control!(
    ext,
    ExtManager,
    ext_data_control_device_v1::ExtDataControlDeviceV1,
    ext_data_control_offer_v1::ExtDataControlOfferV1,
    ext_data_control_source_v1::ExtDataControlSourceV1,
    Ext
);
data_control!(
    wlr,
    WlrManager,
    zwlr_data_control_device_v1::ZwlrDataControlDeviceV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    zwlr_data_control_source_v1::ZwlrDataControlSourceV1,
    Wlr
);

struct Display {
    globals: GlobalList,
    queue: EventQueue<State>,
    seat: wl_seat::WlSeat,
    seat_id: u32,
}
impl Display {
    fn open() -> Result<Self> {
        // Registry probes and transfers need independent connections. Never consume
        // the toolkit's inherited, single-use WAYLAND_SOCKET descriptor.
        if std::env::var_os("WAYLAND_SOCKET").is_some() {
            return Err(ClipboardError::UnsupportedBackend);
        }
        let name = std::env::var_os("WAYLAND_DISPLAY")
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "wayland-0".into());
        let mut socket = std::path::PathBuf::from(name);
        if socket.is_relative() {
            let root = std::env::var_os("XDG_RUNTIME_DIR")
                .map(std::path::PathBuf::from)
                .filter(|root| root.is_absolute())
                .ok_or(ClipboardError::UnsupportedBackend)?;
            socket = root.join(socket);
        }
        let stream = std::os::unix::net::UnixStream::connect(socket)
            .map_err(|_| ClipboardError::UnsupportedBackend)?;
        let conn =
            Connection::from_socket(stream).map_err(|_| ClipboardError::UnsupportedBackend)?;
        let (globals, mut queue) =
            registry_queue_init(&conn).map_err(|_| ClipboardError::UnsupportedBackend)?;
        let items = globals.contents().clone_list();
        if !items.iter().any(|g| {
            g.interface == ExtManager::interface().name
                || g.interface == WlrManager::interface().name
        }) {
            return Err(ClipboardError::UnsupportedBackend);
        }
        let mut seats: Vec<_> = items
            .iter()
            .filter(|g| g.interface == "wl_seat")
            .map(|g| {
                let seat = globals.registry().bind::<wl_seat::WlSeat, _, _>(
                    g.name,
                    g.version.min(2),
                    &queue.handle(),
                    SeatName::default(),
                );
                (g.name, seat)
            })
            .collect();
        if seats.len() > 1 {
            let expected =
                std::env::var("XDG_SEAT").map_err(|_| ClipboardError::UnsupportedBackend)?;
            queue.roundtrip(&mut State::default()).map_err(access)?;
            seats.retain(|(_, seat)| {
                seat.data::<SeatName>().is_some_and(|data| {
                    data.lock()
                        .is_ok_and(|name| name.as_deref() == Some(&expected))
                })
            });
        }
        if seats.len() != 1 {
            return Err(ClipboardError::UnsupportedBackend);
        }
        let (seat_id, seat) = seats.pop().unwrap();
        Ok(Self {
            globals,
            queue,
            seat,
            seat_id,
        })
    }

    fn attach(&mut self, state: &mut State) -> Result<()> {
        let q = self.queue.handle();
        state.seat_id = Some(self.seat_id);
        macro_rules! attach {
            ($manager:expr) => {{
                let manager = $manager;
                let device = manager.get_data_device(&self.seat, &q, ());
                self.queue.roundtrip(state).map_err(access)?;
                if state.finished {
                    return Err(ClipboardError::UnsupportedBackend);
                }
                if state.published.is_some() {
                    let before = state.generation;
                    let source = manager.create_data_source(&q, ());
                    source.offer(MIME.into());
                    source.offer("text/plain".into());
                    device.set_selection(Some(&source));
                    self.queue.roundtrip(state).map_err(access)?;
                    if state.finished || state.generation == before || state.selection.is_none() {
                        return Err(ClipboardError::Changed);
                    }
                }
            }};
        }
        if let Ok(manager) = self.globals.bind::<ExtManager, _, _>(&q, 1..=1, ()) {
            attach!(manager);
        } else {
            attach!(self
                .globals
                .bind::<WlrManager, _, _>(&q, 1..=1, ())
                .map_err(access)?);
        }
        Ok(())
    }
}

pub(super) fn available() -> bool {
    Display::open().is_ok()
}

fn nonblocking(fd: &OwnedFd) -> Result<()> {
    fcntl_setfl(fd, fcntl_getfl(fd).map_err(access)? | OFlags::NONBLOCK).map_err(access)
}

struct Paste {
    pipe: File,
    sent: usize,
    deadline: Instant,
}

impl Paste {
    fn pending(&mut self, bytes: &[u8]) -> bool {
        while self.sent < bytes.len() && Instant::now() < self.deadline {
            match self.pipe.write(&bytes[self.sent..]) {
                Ok(0) => break,
                Ok(n) => self.sent += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
                Err(_) => break,
            }
        }
        false
    }
}

// Keep serving the same bytes after the popup closes. An abandoned paste must
// neither block another reader nor delay the compositor's cancellation event.
fn serve(queue: &mut EventQueue<State>, state: &mut State) -> Result<()> {
    loop {
        queue.dispatch_pending(state).map_err(access)?;
        if state.finished {
            return Ok(());
        }
        let text = state.published.as_ref().ok_or(ClipboardError::Access)?;
        state
            .pastes
            .retain_mut(|paste| paste.pending(text.as_bytes()));
        if state.pastes.is_empty() {
            queue.blocking_dispatch(state).map_err(access)?;
        } else if let Some(guard) = queue.prepare_read() {
            let mut fds = vec![PollFd::from_borrowed_fd(
                guard.connection_fd(),
                PollFlags::IN,
            )];
            fds.extend(
                state
                    .pastes
                    .iter()
                    .map(|paste| PollFd::new(&paste.pipe, PollFlags::OUT)),
            );
            poll(&mut fds, Some(&TICK)).map_err(access)?;
            if !fds[0].revents().is_empty() {
                guard.read().map_err(access)?;
            }
        }
    }
}

pub(super) fn read(cancelled: &dyn Fn() -> bool) -> Result<Zeroizing<String>> {
    let mut display = Display::open()?;
    let mut state = State::default();
    display.attach(&mut state)?;
    let generation = state.generation;
    let offer = state.selection.as_ref().ok_or(ClipboardError::NoText)?;
    let (read, write) = pipe_with(PipeFlags::CLOEXEC).map_err(access)?;
    nonblocking(&read)?;
    offer.receive(write.as_fd())?;
    drop(write);
    display.queue.flush().map_err(access)?;
    let mut pipe = File::from(read);
    let mut bytes = Zeroizing::new(Vec::new());
    let mut buf = Zeroizing::new([0u8; 8192]);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        check(deadline, cancelled)?;
        display.queue.dispatch_pending(&mut state).map_err(access)?;
        if state.finished || state.generation != generation {
            return Err(ClipboardError::Changed);
        }
        match pipe.read(&mut *buf) {
            Ok(0) => break,
            Ok(n) if n > MAX_TEXT_BYTES - bytes.len() => return Err(ClipboardError::TooLarge),
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                continue;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            Err(_) => return Err(ClipboardError::Access),
        }
        if let Some(guard) = display.queue.prepare_read() {
            let mut fds = [
                PollFd::from_borrowed_fd(guard.connection_fd(), PollFlags::IN),
                PollFd::new(&pipe, PollFlags::IN),
            ];
            poll(&mut fds, Some(&TICK)).map_err(access)?;
            if !fds[0].revents().is_empty() {
                guard.read().map_err(access)?;
            }
        }
    }
    display.queue.roundtrip(&mut state).map_err(access)?;
    check(deadline, cancelled)?;
    if state.finished || state.generation != generation {
        return Err(ClipboardError::Changed);
    }
    utf8(bytes)
}

pub(super) fn write(text: &str) -> Result<()> {
    let mut display = Display::open()?;
    let mut state = State {
        published: Some(Zeroizing::new(text.to_owned())),
        ..State::default()
    };
    let (ack, ready) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("clipboard-wayland-owner".into())
        .spawn(move || {
            let result = display.attach(&mut state);
            let claimed = result.is_ok();
            if ack.send(result).is_err() || !claimed {
                return;
            }
            let _ = serve(&mut display.queue, &mut state);
        })
        .map_err(access)?;
    ready.recv().map_err(access)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires SDM_DISPOSABLE_CLIPBOARD=1 and an isolated data-control compositor"]
    fn slow_paste_releases_its_source_on_replacement() {
        assert_eq!(
            std::env::var("SDM_DISPOSABLE_CLIPBOARD").as_deref(),
            Ok("1")
        );
        let text = "a".repeat(MAX_TEXT_BYTES);
        write(&text).unwrap();
        let mut client = Display::open().unwrap();
        let mut state = State::default();
        client.attach(&mut state).unwrap();
        let (reader, sender) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        rustix::pipe::fcntl_setpipe_size(&reader, 4096).unwrap();
        state
            .selection
            .as_ref()
            .unwrap()
            .receive(sender.as_fd())
            .unwrap();
        drop(sender);
        client.queue.flush().unwrap();
        let mut ready = [PollFd::new(&reader, PollFlags::IN)];
        assert_eq!(
            poll(
                &mut ready,
                Some(&Timespec {
                    tv_sec: 2,
                    tv_nsec: 0
                })
            )
            .unwrap(),
            1
        );
        // A slow paste has filled this pipe. A second receiver must still get the whole item.
        assert_eq!(*read(&|| false).unwrap(), text);
        write("replacement").unwrap();
        assert_eq!(read(&|| false).unwrap().as_str(), "replacement");
        client.queue.roundtrip(&mut state).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let mut old = Vec::new();
        File::from(reader).read_to_end(&mut old).unwrap();
        assert!(old.len() < MAX_TEXT_BYTES && old.iter().all(|b| *b == b'a'));
    }
}
