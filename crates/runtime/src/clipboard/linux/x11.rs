use super::{access, check, utf8, ClipboardError, Result, Zeroizing, TIMEOUT};
use crate::clipboard::MAX_TEXT_BYTES;
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use std::{sync::mpsc, thread, time::Instant};
use x11rb::{
    connection::{Connection, RequestConnection},
    protocol::{
        xfixes::{ConnectionExt as _, SelectionEventMask},
        xproto::*,
        Event,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    CURRENT_TIME, NONE,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        CLIPBOARD, TARGETS, TIMESTAMP, UTF8_STRING, INCR, SDM_CLIPBOARD,
        utf8: b"text/plain;charset=utf-8", plain: b"text/plain",
    }
}

fn connect() -> Result<(RustConnection, usize)> {
    let (conn, screen) = x11rb::connect(None).map_err(|_| ClipboardError::UnsupportedBackend)?;
    conn.xfixes_query_version(2, 0)
        .map_err(access)?
        .reply()
        .map_err(|_| ClipboardError::UnsupportedBackend)?;
    Ok((conn, screen))
}

pub(super) fn available() -> bool {
    connect().is_ok()
}

struct Board {
    conn: RustConnection,
    window: Window,
    atoms: Atoms,
}

impl Board {
    fn open() -> Result<Self> {
        let (conn, screen) = connect()?;
        let root = conn.setup().roots[screen].root;
        let window = conn.generate_id().map_err(access)?;
        conn.create_window(
            0,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(access)?
        .check()
        .map_err(access)?;
        let atoms = Atoms::new(&conn).map_err(access)?.reply().map_err(access)?;
        Ok(Self {
            conn,
            window,
            atoms,
        })
    }

    fn owner(&self) -> Result<Window> {
        Ok(self
            .conn
            .get_selection_owner(self.atoms.CLIPBOARD)
            .map_err(access)?
            .reply()
            .map_err(access)?
            .owner)
    }

    fn text_types(&self) -> [Atom; 3] {
        [self.atoms.UTF8_STRING, self.atoms.utf8, self.atoms.plain]
    }

    fn wait(
        &self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
        accept: impl Fn(&Event) -> bool,
    ) -> Result<Event> {
        loop {
            check(deadline, cancelled)?;
            match self.conn.poll_for_event().map_err(access)? {
                Some(Event::XfixesSelectionNotify(_)) => return Err(ClipboardError::Changed),
                Some(event) if accept(&event) => return Ok(event),
                Some(_) => (),
                None => {
                    let mut fd = [PollFd::new(self.conn.stream(), PollFlags::IN)];
                    poll(
                        &mut fd,
                        Some(&Timespec {
                            tv_sec: 0,
                            tv_nsec: 25_000_000,
                        }),
                    )
                    .map_err(access)?;
                }
            }
        }
    }

    fn convert(&self, target: Atom, deadline: Instant, cancelled: &dyn Fn() -> bool) -> Result<()> {
        self.conn
            .convert_selection(
                self.window,
                self.atoms.CLIPBOARD,
                target,
                self.atoms.SDM_CLIPBOARD,
                CURRENT_TIME,
            )
            .map_err(access)?
            .check()
            .map_err(access)?;
        let event = self.wait(deadline, cancelled, |e| matches!(e, Event::SelectionNotify(n)
            if n.requestor == self.window && n.selection == self.atoms.CLIPBOARD && n.target == target))?;
        if matches!(event, Event::SelectionNotify(n) if n.property == self.atoms.SDM_CLIPBOARD) {
            Ok(())
        } else {
            Err(ClipboardError::UnsupportedFormat)
        }
    }

    fn property(&self, max_bytes: usize) -> Result<GetPropertyReply> {
        let reply = self
            .conn
            .get_property(
                true,
                self.window,
                self.atoms.SDM_CLIPBOARD,
                AtomEnum::ANY,
                0,
                max_bytes.div_ceil(4) as u32,
            )
            .map_err(access)?
            .reply()
            .map_err(access)?;
        if reply.bytes_after != 0 || reply.value.len() > max_bytes {
            return Err(ClipboardError::TooLarge);
        }
        Ok(reply)
    }

    fn contents(
        &self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Zeroizing<String>> {
        self.convert(self.atoms.TARGETS, deadline, cancelled)?;
        let targets = self.property(4096)?;
        if targets.type_ != u32::from(AtomEnum::ATOM) {
            return Err(ClipboardError::UnsupportedFormat);
        }
        let target = self
            .text_types()
            .into_iter()
            .find(|t| targets.value32().is_some_and(|mut v| v.any(|a| a == *t)))
            .ok_or(ClipboardError::UnsupportedFormat)?;
        self.convert(target, deadline, cancelled)?;
        let mut part = self.property(MAX_TEXT_BYTES)?;
        if part.type_ != self.atoms.INCR {
            if part.type_ != target || part.format != 8 {
                return Err(ClipboardError::UnsupportedFormat);
            }
            return utf8(Zeroizing::new(part.value));
        }
        if part
            .value32()
            .and_then(|mut v| v.next())
            .is_some_and(|n| n as usize > MAX_TEXT_BYTES)
        {
            return Err(ClipboardError::TooLarge);
        }
        let mut text = Zeroizing::new(Vec::new());
        loop {
            self.wait(deadline, cancelled, |e| matches!(e, Event::PropertyNotify(n)
                if n.window == self.window && n.atom == self.atoms.SDM_CLIPBOARD && n.state == Property::NEW_VALUE))?;
            part = self.property(MAX_TEXT_BYTES - text.len())?;
            if part.type_ != target || part.format != 8 {
                return Err(ClipboardError::UnsupportedFormat);
            }
            if part.value.is_empty() {
                return utf8(text);
            }
            text.extend_from_slice(&Zeroizing::new(part.value));
        }
    }

    fn claim(&self) -> Result<Timestamp> {
        self.conn
            .change_property8(
                PropMode::REPLACE,
                self.window,
                self.atoms.SDM_CLIPBOARD,
                AtomEnum::INTEGER,
                &[0],
            )
            .map_err(access)?
            .check()
            .map_err(access)?;
        let timestamp = self.wait(Instant::now() + TIMEOUT, &|| false, |event| {
            matches!(event, Event::PropertyNotify(n) if n.window == self.window && n.atom == self.atoms.SDM_CLIPBOARD)
        })?;
        let Event::PropertyNotify(stamp) = timestamp else {
            unreachable!()
        };
        self.conn
            .set_selection_owner(self.window, self.atoms.CLIPBOARD, stamp.time)
            .map_err(access)?
            .check()
            .map_err(access)?;
        if self.owner()? != self.window {
            return Err(ClipboardError::Changed);
        }
        Ok(stamp.time)
    }

    fn serve(&self, request: SelectionRequestEvent, stamp: Timestamp, text: &str) -> Result<()> {
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let a = &self.atoms;
        let target = request.target;
        let supported = if request.selection != a.CLIPBOARD || request.owner != self.window {
            false
        } else if target == a.TARGETS || target == a.TIMESTAMP {
            let targets = [a.TARGETS, a.TIMESTAMP, a.UTF8_STRING, a.utf8, a.plain];
            let (ty, data) = if target == a.TARGETS {
                (AtomEnum::ATOM, targets.as_slice())
            } else {
                (AtomEnum::INTEGER, std::slice::from_ref(&stamp))
            };
            self.conn
                .change_property32(PropMode::REPLACE, request.requestor, property, ty, data)
                .map_err(access)?
                .check()
                .is_ok()
        } else if self.text_types().contains(&target) {
            self.conn
                .change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    target,
                    text.as_bytes(),
                )
                .map_err(access)?
                .check()
                .is_ok()
        } else {
            false
        };
        let reply = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target,
            property: if supported { property } else { NONE },
        };
        self.conn
            .send_event(false, request.requestor, EventMask::NO_EVENT, reply)
            .map_err(access)?;
        self.conn.flush().map_err(access)
    }
}

pub(super) fn read(cancelled: &dyn Fn() -> bool) -> Result<Zeroizing<String>> {
    let board = Board::open()?;
    board
        .conn
        .xfixes_select_selection_input(
            board.window,
            board.atoms.CLIPBOARD,
            SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )
        .map_err(access)?
        .check()
        .map_err(access)?;
    let owner = board.owner()?;
    if owner == NONE {
        return Err(ClipboardError::NoText);
    }
    let deadline = Instant::now() + TIMEOUT;
    let result = board.contents(deadline, cancelled);
    check(deadline, cancelled)?;
    if board.owner()? != owner {
        return Err(ClipboardError::Changed);
    }
    // The preceding reply is a server barrier, including changes away and back to the same window.
    while let Some(event) = board.conn.poll_for_event().map_err(access)? {
        check(deadline, cancelled)?;
        if matches!(event, Event::XfixesSelectionNotify(_)) {
            return Err(ClipboardError::Changed);
        }
    }
    result
}

pub(super) fn write(text: &str) -> Result<()> {
    let board = Board::open()?;
    if board.conn.maximum_request_bytes() < text.len() + 32 {
        return Err(ClipboardError::TooLarge);
    }
    let text = Zeroizing::new(text.to_owned());
    let (ack, ready) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("clipboard-x11-owner".into())
        .spawn(move || {
            let stamp = match board.claim() {
                Ok(stamp) => stamp,
                Err(error) => {
                    let _ = ack.send(Err(error));
                    return;
                }
            };
            if ack.send(Ok(())).is_err() {
                return;
            }
            while let Ok(event) = board.conn.wait_for_event() {
                match event {
                    Event::SelectionClear(_) => break,
                    Event::SelectionRequest(request)
                        if board.serve(request, stamp, &text).is_err() =>
                    {
                        break;
                    }
                    _ => {}
                }
            }
        })
        .map_err(access)?;
    ready.recv().map_err(access)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires SDM_DISPOSABLE_CLIPBOARD=1 and an isolated X server"]
    fn incremental_selection_is_bounded_across_chunks() {
        assert_eq!(
            std::env::var("SDM_DISPOSABLE_CLIPBOARD").as_deref(),
            Ok("1")
        );
        assert!(matches!(
            super::super::session(),
            Ok(super::super::Session::X11)
        ));
        for extra in [false, true] {
            let board = Board::open().unwrap();
            let stamp = board.claim().unwrap();
            thread::scope(|scope| {
                let (finish, finished) = mpsc::channel();
                scope.spawn(move || {
                    let a = &board.atoms;
                    let deadline = Instant::now() + TIMEOUT;
                    loop {
                        let event = board.wait(deadline, &|| false, |event| matches!(event, Event::SelectionRequest(_))).unwrap();
                        let Event::SelectionRequest(req) = event else { unreachable!() };
                        if req.target != a.UTF8_STRING {
                            board.serve(req, stamp, "").unwrap();
                            continue;
                        }
                        let conn = &board.conn;
                        conn.change_window_attributes(req.requestor, &ChangeWindowAttributesAux::new()
                            .event_mask(EventMask::PROPERTY_CHANGE)).unwrap().check().unwrap();
                        // An INCR estimate is a lower bound, not the actual transfer length.
                        conn.change_property32(PropMode::REPLACE, req.requestor, req.property, a.INCR, &[1]).unwrap();
                        conn.send_event(false, req.requestor, EventMask::NO_EVENT, SelectionNotifyEvent {
                            response_type: SELECTION_NOTIFY_EVENT, time: req.time, requestor: req.requestor,
                            selection: req.selection, target: req.target, property: req.property, sequence: 0,
                        }).unwrap();
                        conn.flush().unwrap();
                        let chunk = vec![b'x'; MAX_TEXT_BYTES / 2];
                        let mut parts = vec![chunk.as_slice(), chunk.as_slice()];
                        if extra { parts.push(b"x"); } else { parts.push(b""); }
                        for part in parts {
                            board.wait(deadline, &|| false, |event| matches!(event, Event::PropertyNotify(n)
                                if n.window == req.requestor && n.atom == req.property && n.state == Property::DELETE)).unwrap();
                            conn.change_property8(PropMode::REPLACE, req.requestor, req.property, req.target, part).unwrap();
                            conn.flush().unwrap();
                        }
                        let _ = finished.recv_timeout(TIMEOUT);
                        break;
                    }
                });
                let result = read(&|| false);
                finish.send(()).unwrap();
                if extra {
                    assert_eq!(result.unwrap_err(), ClipboardError::TooLarge);
                } else {
                    assert_eq!(result.unwrap().as_bytes(), vec![b'x'; MAX_TEXT_BYTES]);
                }
            });
        }
    }
}
