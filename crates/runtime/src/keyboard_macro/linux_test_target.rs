//! Scoped X11 scratch selection, not an authorization or qualification bypass
//! XRes supplies the client PID, never _NET_WM_PID. A pidfd and pinned procfs
//! directory prevent adopting a later process with the same PID. StructureNotify
//! is installed before accepting identity; destruction remains sticky across XID
//! reuse. Confirmation may leave the target; strict focus observation starts only
//! at playback. No server grab, focus change, key capture, or input is performed
//!
//! Target continuity is the selected live window and its X-server client resource
//! range, not a claim about the original process owning a transferable connection
//! XRes PID/procfs checks remain conservative metadata/liveness checks only. They
//! cannot attest the original process generation. Window/client reuse is rejected
//! by sticky lifetime events, subscribed before identity acceptance and fenced
//! before each check. This trusts the local X server, not arbitrary peer clients
//! Admission requires a live desktop-session lease. Native qualification is
//! separate evidence, not an unconditional implementation switch
//! Sources: x.org/releases/current/doc/resourceproto/resproto.txt;
//! man7.org/linux/man-pages/man2/pidfd_open.2.html;
//! man7.org/linux/man-pages/man5/proc_pid_status.5.html;
//! x.org/releases/current/doc/xproto/x11protocol.html

const MAX_ANCESTRY: usize = 32;

// Construct the Linux test target error result
fn error() -> String {
    "The X11 scratch target is no longer verifiable. Select it again.".into()
}

// Read exactly one named field from the proc status text
fn field<'a>(text: &'a str, name: &str) -> Result<&'a str, String> {
    let mut values = text.lines().filter_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key == name).then_some(value.trim())
    });
    let value = values.next().ok_or_else(error)?;
    if values.next().is_some() {
        return Err(error());
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Credentials {
    pid: u32,
    uids: [u32; 4],
    gids: [u32; 4],
    groups: Vec<u32>,
    capabilities: [u64; 4],
}
impl Credentials {
    // Parse the supplied Linux test target representation
    fn parse(text: &str) -> Result<Self, String> {
        // Parse exactly four numeric process identity fields
        fn ids(text: &str) -> Result<[u32; 4], String> {
            text.split_whitespace()
                .map(|value| value.parse().map_err(|_| error()))
                .collect::<Result<Vec<_>, _>>()?
                .try_into()
                .map_err(|_| error())
        }
        let pid = field(text, "Pid")?.parse().map_err(|_| error())?;
        let tgid: u32 = field(text, "Tgid")?.parse().map_err(|_| error())?;
        if pid == 0 || pid != tgid {
            return Err(error());
        }
        let groups = field(text, "Groups")?
            .split_whitespace()
            .map(|value| value.parse().map_err(|_| error()))
            .collect::<Result<Vec<_>, _>>()?;
        if groups.len() > 256 {
            return Err(error());
        }
        let mut capabilities = [0; 4];
        for (index, name) in ["CapInh", "CapPrm", "CapEff", "CapAmb"].iter().enumerate() {
            capabilities[index] =
                u64::from_str_radix(field(text, name)?, 16).map_err(|_| error())?;
        }
        Ok(Self {
            pid,
            uids: ids(field(text, "Uid")?)?,
            gids: ids(field(text, "Gid")?)?,
            groups,
            capabilities,
        })
    }
    // Report the ordinary condition for Linux test target
    fn ordinary(&self, owner: &Self) -> bool {
        let uid = owner.uids[0];
        let gid = owner.gids[0];
        uid != 0
            && owner.uids == [uid; 4]
            && owner.gids == [gid; 4]
            && owner.capabilities == [0; 4]
            && self.uids == [uid; 4]
            && self.gids == [gid; 4]
            && self.capabilities == [0; 4]
            && self.groups.iter().all(|group| owner.groups.contains(group))
    }
}

// Read the process start time after validating the PID in proc stat
fn process_generation(stat: &str, pid: u32) -> Result<u64, String> {
    let (prefix, tail) = stat.rsplit_once(')').ok_or_else(error)?;
    let (actual, _) = prefix.split_once(" (").ok_or_else(error)?;
    if actual.parse::<u32>().map_err(|_| error())? != pid {
        return Err(error());
    }
    // The comm field may contain spaces or parentheses. Fields after its final
    // ')' start at state (3); starttime is field 22, not a whitespace offset in comm.
    let fields: Vec<_> = tail.split_whitespace().take(20).collect();
    if fields.len() != 20 || !matches!(fields[0], "R" | "S" | "D" | "I") {
        return Err(error());
    }
    fields[19].parse().map_err(|_| error())
}

// Report the valid chain condition for Linux test target
fn valid_chain(chain: &[u32], target: u32, root: u32) -> bool {
    !chain.is_empty()
        && chain.len() <= MAX_ANCESTRY
        && chain.last() == Some(&target)
        && chain.iter().enumerate().all(|(index, window)| {
            *window > 1 && *window != root && !chain[..index].contains(window)
        })
}

/// X-server resource ownership, not process identity. A range alone can be
/// recycled, so callers must also retain the subscribed window lifetime
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClientRange {
    base: u32,
    mask: u32,
}
impl ClientRange {
    // Report the contains condition for Linux test target
    fn contains(self, window: u32) -> bool {
        self.base > 1
            && self.mask != 0
            && self.base & self.mask == 0
            && window & !self.mask == self.base
    }
}
// Require exactly one X11 client range to own the target window
fn unique_client(window: u32, ranges: &[ClientRange]) -> Result<ClientRange, String> {
    if window <= 1 || ranges.len() > 4096 {
        return Err(error());
    }
    let mut matches = ranges
        .iter()
        .copied()
        .filter(|range| range.contains(window));
    let result = matches.next().ok_or_else(error)?;
    if matches.next().is_some() {
        return Err(error());
    }
    Ok(result)
}

#[derive(Default)]
struct Lifetime {
    invalid: bool,
    focus_armed: bool,
}
impl Lifetime {
    // Invalidate the target after its lifetime can no longer be verified
    fn lost(&mut self) {
        self.invalid = true;
    }
    // Invalidate a focus-armed target when focus changes
    fn focus_changed(&mut self) {
        self.invalid |= self.focus_armed;
    }
    // Check the current Linux test target conditions
    fn check(&self) -> Result<(), String> {
        if self.invalid {
            Err(error())
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) use native::{capture, unavailable_reason};

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use crate::keyboard_macro::{
        platform::{linux_session::native as session, x11_connection},
        test_target::TestTarget,
    };
    use rustix::{
        event::{poll, PollFd, PollFlags, Timespec},
        fd::OwnedFd,
        fs::{self, AtFlags, Mode, OFlags},
        process::{pidfd_open, Pid, PidfdFlags},
    };
    use std::{fs::File, io::Read, path::Path, time::Duration};
    use x11rb::{
        connection::Connection as _,
        protocol::{
            res::{self, ClientIdMask, ClientIdSpec},
            xproto::{
                AtomEnum, ChangeWindowAttributesAux, ConnectionExt, EventMask, MapState,
                WindowClass,
            },
            Event,
        },
    };

    const BUDGET: Duration = Duration::from_millis(75);
    const PROC_LIMIT: u64 = 16 * 1024;

    // Open the target proc directory without following links and verify procfs
    fn proc_directory(pid: u32) -> Result<OwnedFd, String> {
        let directory = fs::open(
            format!("/proc/{pid}"),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| error())?;
        if fs::fstatfs(&directory).map_err(|_| error())?.f_type != fs::PROC_SUPER_MAGIC {
            return Err(error());
        }
        Ok(directory)
    }
    // Read bounded text relative to the pinned proc directory
    fn proc_text(directory: &OwnedFd, name: &str) -> Result<String, String> {
        let fd = fs::openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| error())?;
        let mut text = String::new();
        File::from(fd)
            .take(PROC_LIMIT + 1)
            .read_to_string(&mut text)
            .map_err(|_| error())?;
        if text.len() as u64 > PROC_LIMIT {
            return Err(error());
        }
        Ok(text)
    }
    // Read the device and inode identity of a proc entry
    fn inode(directory: &OwnedFd, name: &str) -> Result<(u64, u64), String> {
        let stat = fs::statat(directory, name, AtFlags::empty()).map_err(|_| error())?;
        Ok((stat.st_dev, stat.st_ino))
    }
    // Require matching PID and user namespaces for the target and Manager
    fn same_namespaces(left: &OwnedFd, right: &OwnedFd) -> Result<(), String> {
        for name in ["ns/pid", "ns/user"] {
            if inode(left, name)? != inode(right, name)? {
                return Err(
                    "Scratch targets require the same verifiable PID and user namespaces.".into(),
                );
            }
        }
        Ok(())
    }
    // Check that the pinned process descriptor has not signaled exit
    fn alive(fd: &OwnedFd) -> Result<(), String> {
        let mut fds = [PollFd::new(fd, PollFlags::IN)];
        match poll(
            &mut fds,
            Some(&Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            }),
        ) {
            Ok(0) if fds[0].revents().is_empty() => Ok(()),
            _ => Err(error()),
        }
    }
    struct Process {
        pid: u32,
        pidfd: OwnedFd,
        directory: OwnedFd,
        own_directory: OwnedFd,
        generation: u64,
        executable: (u64, u64),
        label: String,
    }
    impl Process {
        // Hold a lease on the selected Linux test target resource
        fn pin(pid: u32, server_pid: u32) -> Result<Self, String> {
            if pid == 0 || pid == std::process::id() || pid == server_pid {
                return Err("Select an ordinary scratch application outside Manager.".into());
            }
            let own_directory = proc_directory(std::process::id())?;
            let server = proc_directory(server_pid)?;
            same_namespaces(&own_directory, &server)?;
            let numeric = i32::try_from(pid)
                .ok()
                .and_then(Pid::from_raw)
                .ok_or_else(error)?;
            let pidfd = pidfd_open(numeric, PidfdFlags::empty()).map_err(|_| {
                "Scratch-target process lifetime monitoring requires Linux pidfd support."
                    .to_string()
            })?;
            alive(&pidfd)?;
            let directory = proc_directory(pid)?;
            let generation = process_generation(&proc_text(&directory, "stat")?, pid)?;
            let executable = inode(&directory, "exe")?;
            if executable == inode(&own_directory, "exe")? {
                return Err("Manager processes cannot be scratch targets.".into());
            }
            let path = fs::readlinkat(&directory, "exe", Vec::new()).map_err(|_| error())?;
            let path = path.to_str().map_err(|_| error())?;
            let label = Path::new(path)
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(error)?
                .to_string();
            let process = Self {
                pid,
                pidfd,
                directory,
                own_directory,
                generation,
                executable,
                label,
            };
            process.check()?;
            Ok(process)
        }
        // Check the current Linux test target conditions
        fn check(&self) -> Result<(), String> {
            alive(&self.pidfd)?;
            same_namespaces(&self.directory, &self.own_directory)?;
            let own = Credentials::parse(&proc_text(&self.own_directory, "status")?)?;
            let target = Credentials::parse(&proc_text(&self.directory, "status")?)?;
            if own.pid != std::process::id()
                || target.pid != self.pid
                || !target.ordinary(&own)
                || process_generation(&proc_text(&self.directory, "stat")?, self.pid)?
                    != self.generation
                || inode(&self.directory, "exe")? != self.executable
            {
                return Err("Select a same-user, non-elevated scratch application with verifiable ownership.".into());
            }
            alive(&self.pidfd)
        }
    }

    struct Target {
        session: Option<session::Lease>,
        connection: Option<x11_connection::Connection>,
        root: u32,
        window: u32,
        active_atom: u32,
        process: Process,
        client: ClientRange,
        label: String,
        lifetime: Lifetime,
        focus_chain: Vec<u32>,
    }
    impl Target {
        // Borrow the retained X11 connection or reject a lost target
        fn connection(&self) -> Result<&x11_connection::Connection, String> {
            self.connection.as_ref().ok_or_else(error)
        }
        // Arm the Linux test target operation for its next event
        fn arm(&self) -> Result<(), String> {
            self.connection()?.stream().arm(BUDGET);
            Ok(())
        }
        // Drain pending Linux test target entries
        fn drain(&mut self) -> Result<(), String> {
            for _ in 0..1024 {
                let Some(event) = self.connection()?.poll_for_event().map_err(|_| error())? else {
                    return self.lifetime.check();
                };
                match event {
                    Event::DestroyNotify(value)
                        if value.window == self.window
                            || self.focus_chain.contains(&value.window) =>
                    {
                        self.lifetime.lost()
                    }
                    Event::UnmapNotify(value) if value.window == self.window => {
                        self.lifetime.lost()
                    }
                    Event::ReparentNotify(value)
                        if value.window == self.window
                            || self.focus_chain.contains(&value.window) =>
                    {
                        self.lifetime.lost()
                    }
                    Event::CreateNotify(value)
                        if self.focus_chain.first() == Some(&value.parent) =>
                    {
                        self.lifetime.lost()
                    }
                    Event::ReparentNotify(value)
                        if self.focus_chain.first() == Some(&value.parent) =>
                    {
                        self.lifetime.lost()
                    }
                    Event::FocusOut(_) => self.lifetime.focus_changed(),
                    Event::PropertyNotify(value)
                        if value.window == self.root && value.atom == self.active_atom =>
                    {
                        self.lifetime.focus_changed()
                    }
                    Event::Error(_) => self.lifetime.lost(),
                    _ => (),
                }
            }
            self.lifetime.lost();
            Err("Scratch-target observer queue overflowed.".into())
        }
        // Fence earlier X11 requests with a reply and drain queued target events
        fn barrier(&mut self) -> Result<(), String> {
            // A reply fences all preceding subscription requests and events
            self.connection()?
                .get_input_focus()
                .map_err(|_| error())?
                .reply()
                .map_err(|_| error())?;
            self.drain()
        }
        // Read current focus chain
        fn current_focus_chain(&self) -> Result<Vec<u32>, String> {
            let connection = self.connection()?;
            if active_window(connection, self.root, self.active_atom)? != self.window {
                return Err("Focus the selected scratch window before test playback.".into());
            }
            let mut window = connection
                .get_input_focus()
                .map_err(|_| error())?
                .reply()
                .map_err(|_| error())?
                .focus;
            // Core X11 can route a key to an inferior pointer window when the
            // focus is its ancestor. Without a pointer-routing observer, accept
            // only a leaf focus window; never assume a foreign embedded child
            // belongs to the selected process. Child creation is subscribed below
            if window <= 1 || window == self.root {
                return Err(error());
            }
            let focused_tree = connection
                .query_tree(window)
                .map_err(|_| error())?
                .reply()
                .map_err(|_| error())?;
            if focused_tree.root != self.root || !focused_tree.children.is_empty() {
                return Err(
                    "X11 scratch playback requires an unambiguous leaf keyboard-focus window."
                        .into(),
                );
            }
            let mut chain = Vec::new();
            for _ in 0..MAX_ANCESTRY {
                if window <= 1 || window == self.root || chain.contains(&window) {
                    return Err(error());
                }
                // Foreign embedded clients must not receive a selected app's test
                if client_range(connection, window)? != self.client {
                    return Err(error());
                }
                chain.push(window);
                if window == self.window {
                    return valid_chain(&chain, self.window, self.root)
                        .then_some(chain)
                        .ok_or_else(error);
                }
                let tree = connection
                    .query_tree(window)
                    .map_err(|_| error())?
                    .reply()
                    .map_err(|_| error())?;
                if tree.root != self.root {
                    return Err(error());
                }
                window = tree.parent;
            }
            Err("The scratch target's keyboard-focus ancestry cannot be verified.".into())
        }
        // Validate target liveness and bind or verify its current focus chain
        fn focused(&mut self) -> Result<(), String> {
            self.check_alive()?;
            self.arm()?;
            let chain = self.current_focus_chain()?;
            if self.lifetime.focus_armed {
                if chain != self.focus_chain {
                    return Err(error());
                }
            } else {
                // Keep focus-child lifetime/focus notifications scoped to this
                // connection. A child destroyed during installation fails closed
                for (index, window) in chain.iter().enumerate() {
                    let mut mask = EventMask::STRUCTURE_NOTIFY | EventMask::FOCUS_CHANGE;
                    if index == 0 {
                        mask |= EventMask::SUBSTRUCTURE_NOTIFY;
                    }
                    watch(self.connection()?, *window, mask)?;
                }
                self.focus_chain = chain;
                self.barrier()?;
                if self.current_focus_chain()? != self.focus_chain {
                    return Err(error());
                }
                // Old confirmation-trip focus events have now been drained
                self.lifetime.focus_armed = true;
            }
            self.barrier()?;
            self.process.check()?;
            self.session.as_ref().ok_or_else(error)?.check_current()
        }
    }
    impl TestTarget for Target {
        // Return the display label for Linux test target
        fn label(&self) -> &str {
            &self.label
        }
        // Reject an input target whose original process is no longer alive
        fn check_alive(&mut self) -> Result<(), String> {
            let result = (|| {
                self.lifetime.check()?;
                self.session.as_ref().ok_or_else(error)?.check()?;
                self.arm()?;
                self.barrier()?;
                self.process.check()?;
                visible(self.connection()?, self.window)?;
                if client_range(self.connection()?, self.window)? != self.client
                    || client_pid(self.connection()?, self.window)? != self.process.pid
                {
                    return Err(error());
                }
                self.barrier()?;
                self.session.as_ref().ok_or_else(error)?.check_current()
            })();
            if result.is_err() {
                self.lifetime.lost();
            }
            result
        }
        // Reject an input target that no longer holds the required focus
        fn check_focused(&mut self) -> Result<(), String> {
            let result = self.focused();
            if result.is_err() {
                self.lifetime.lost();
            }
            result
        }
        // Finalize the pending Linux test target operation
        fn finish(&mut self) -> Result<(), String> {
            self.lifetime.lost();
            // Closing this private connection removes all its event selections,
            // including a vanished target. No shared mask restoration is needed
            self.connection.take();
            self.session.take();
            Ok(())
        }
    }
    // Watch for Linux test target changes
    fn watch(
        connection: &x11_connection::Connection,
        window: u32,
        mask: EventMask,
    ) -> Result<(), String> {
        connection
            .change_window_attributes(window, &ChangeWindowAttributesAux::new().event_mask(mask))
            .map_err(|_| error())?
            .check()
            .map_err(|_| error())
    }
    // Look up an existing X11 atom and reject a missing identifier
    fn atom(connection: &x11_connection::Connection, name: &[u8]) -> Result<u32, String> {
        let value = connection
            .intern_atom(true, name)
            .map_err(|_| error())?
            .reply()
            .map_err(|_| error())?
            .atom;
        (value != 0).then_some(value).ok_or_else(error)
    }
    // Read and validate the window reported by the root active-window property
    fn active_window(
        connection: &x11_connection::Connection,
        root: u32,
        active: u32,
    ) -> Result<u32, String> {
        let reply = connection
            .get_property(false, root, active, AtomEnum::WINDOW, 0, 1)
            .map_err(|_| error())?
            .reply()
            .map_err(|_| error())?;
        if reply.type_ != u32::from(AtomEnum::WINDOW)
            || reply.format != 32
            || reply.bytes_after != 0
            || reply.value.len() != 4
        {
            return Err(error());
        }
        reply
            .value32()
            .and_then(|mut values| values.next())
            .filter(|value| *value > 1 && *value != root)
            .ok_or_else(error)
    }
    // Require a viewable input/output target window
    fn visible(connection: &x11_connection::Connection, window: u32) -> Result<(), String> {
        let attributes = connection
            .get_window_attributes(window)
            .map_err(|_| error())?
            .reply()
            .map_err(|_| error())?;
        if attributes.map_state != MapState::VIEWABLE
            || attributes.class != WindowClass::INPUT_OUTPUT
            || attributes.override_redirect
        {
            return Err(error());
        }
        Ok(())
    }
    // Find the X11 resource range owning the target window
    fn client_range(
        connection: &x11_connection::Connection,
        window: u32,
    ) -> Result<ClientRange, String> {
        let reply = res::query_clients(connection)
            .map_err(|_| error())?
            .reply()
            .map_err(|_| error())?;
        let ranges: Vec<_> = reply
            .clients
            .iter()
            .map(|client| ClientRange {
                base: client.resource_base,
                mask: client.resource_mask,
            })
            .collect();
        unique_client(window, &ranges)
    }
    // Resolve the local process owning the X11 client
    fn client_pid(connection: &x11_connection::Connection, window: u32) -> Result<u32, String> {
        let reply = res::query_client_ids(
            connection,
            &[ClientIdSpec {
                client: window,
                mask: ClientIdMask::LOCAL_CLIENT_PID,
            }],
        )
        .map_err(|_| error())?
        .reply()
        .map_err(|_| error())?;
        // XRes masks the resource to the client. Do not compare spec.client to
        // window, and do not treat a missing/ambiguous result as a remote PID
        if reply.ids.len() != 1
            || reply.ids[0].spec.mask != ClientIdMask::LOCAL_CLIENT_PID
            || reply.ids[0].value.len() != 1
        {
            return Err("XRes cannot verify a unique local owner for the scratch window.".into());
        }
        let pid = reply.ids[0].value[0];
        (pid > 0 && pid <= i32::MAX as u32)
            .then_some(pid)
            .ok_or_else(error)
    }
    // Explain why the current operation is unavailable
    pub(crate) fn unavailable_reason() -> Option<String> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland")
        {
            return Some(
                crate::keyboard_macro::platform::wayland_limits::scratch_target_unavailable_reason(
                )
                .into(),
            );
        }
        if std::env::var_os("DISPLAY").is_none() {
            return Some("Scratch-target selection requires an interactive X11 desktop. Use simulation in headless sessions.".into());
        }
        session::readiness().err()
    }
    // Capture the current Linux test target observation
    pub(crate) fn capture() -> Result<Box<dyn TestTarget>, String> {
        // Also gate direct callers before opening a display, procfs, or pidfd
        if let Some(reason) = unavailable_reason() {
            return Err(reason);
        }
        let session = session::acquire()?;
        let (connection, screen) = x11_connection::connect()?;
        connection.stream().arm(Duration::from_millis(250));
        let root = connection.setup().roots.get(screen).ok_or_else(error)?.root;
        let version = res::query_version(&connection, 1, 2)
            .map_err(|_| error())?
            .reply()
            .map_err(|_| error())?;
        if (version.server_major, version.server_minor) < (1, 2) {
            return Err(
                "Scratch-target selection requires XRes 1.2 local process identities.".into(),
            );
        }
        if connection
            .query_extension(b"XWAYLAND")
            .map_err(|_| error())?
            .reply()
            .map_err(|_| error())?
            .present
        {
            return Err("XWayland scratch selection is not supported.".into());
        }
        let server_pid = connection.stream().peer_pid()?;
        let active_atom = atom(&connection, b"_NET_ACTIVE_WINDOW")?;
        watch(&connection, root, EventMask::PROPERTY_CHANGE)?;
        let window = active_window(&connection, root, active_atom)?;
        // The sampled XID is only a candidate. Identity and UI acceptance start
        // after this acknowledged subscription, never from an earlier PID/title
        watch(
            &connection,
            window,
            EventMask::STRUCTURE_NOTIFY | EventMask::FOCUS_CHANGE,
        )?;
        visible(&connection, window)?;
        let client = client_range(&connection, window)?;
        let pid = client_pid(&connection, window)?;
        let process = Process::pin(pid, server_pid)?;
        let title_atom = atom(&connection, b"_NET_WM_NAME")?;
        let utf8 = atom(&connection, b"UTF8_STRING")?;
        let title = connection
            .get_property(false, window, title_atom, utf8, 0, 256)
            .map_err(|_| error())?
            .reply()
            .map_err(|_| error())?;
        if title.type_ != utf8
            || title.format != 8
            || title.bytes_after != 0
            || title.value.is_empty()
        {
            return Err("Select a scratch window with a bounded, readable title.".into());
        }
        // This label is presentation only, never an ownership or permission input
        let title = std::str::from_utf8(&title.value).map_err(|_| error())?;
        let label = crate::keyboard_macro::test_target::display_label(
            &format!("X11 window (reported process: {})", process.label),
            title,
        );
        let mut target = Target {
            session: Some(session),
            connection: Some(connection),
            root,
            window,
            active_atom,
            process,
            client,
            label,
            lifetime: Lifetime::default(),
            focus_chain: Vec::new(),
        };
        target.check_alive()?;
        target.arm()?;
        target.current_focus_chain()?;
        target.barrier()?;
        // Deliberately not check_focused(): returning to Manager is permitted
        // until the second countdown has completed and playback arms focus
        Ok(Box::new(target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Return the current Linux test target status
    fn status(pid: u32) -> String {
        format!("Pid:\t{pid}\nTgid:\t{pid}\nUid:\t1000 1000 1000 1000\nGid:\t1000 1000 1000 1000\nGroups:\t1000 10\nCapInh:\t0000\nCapPrm:\t0000\nCapEff:\t0000\nCapAmb:\t0000\n")
    }
    #[test]
    // Verify that client identity is server resource range not numeric PID
    fn client_identity_is_server_resource_range_not_numeric_pid() {
        let a = ClientRange {
            base: 0x200000,
            mask: 0x1fffff,
        };
        let b = ClientRange {
            base: 0x400000,
            mask: 0x1fffff,
        };
        assert_eq!(unique_client(0x200043, &[a, b]).unwrap(), a);
        assert_eq!(unique_client(0x200099, &[a, b]).unwrap(), a);
        assert_eq!(unique_client(0x400043, &[a, b]).unwrap(), b);
        assert!(unique_client(0x200043, &[a, a]).is_err());
        assert!(unique_client(0x600043, &[a, b]).is_err());
        assert!(unique_client(1, &[a]).is_err());
        assert!(unique_client(
            0x200043,
            &[ClientRange {
                base: 0x200001,
                ..a
            }]
        )
        .is_err());
        // Even an identical range/window after destruction cannot restore a lease
        let mut lifetime = Lifetime::default();
        lifetime.lost();
        assert_eq!(unique_client(0x200043, &[a]).unwrap(), a);
        assert!(lifetime.check().is_err());
    }
    #[test]
    // Verify that target credentials require same user without saved or capability privilege
    fn target_credentials_require_same_user_without_saved_or_capability_privilege() {
        let own = Credentials::parse(&status(1)).unwrap();
        let mut target = Credentials::parse(&status(2)).unwrap();
        assert!(target.ordinary(&own));
        target.uids[2] = 0;
        assert!(!target.ordinary(&own));
        target.uids[2] = 1000;
        target.capabilities[0] = 1;
        assert!(!target.ordinary(&own));
        target.capabilities[0] = 0;
        target.groups.push(42);
        assert!(!target.ordinary(&own));
        let root = Credentials {
            uids: [0; 4],
            ..own.clone()
        };
        assert!(!root.ordinary(&root));
    }
    #[test]
    // Verify that missing duplicate or malformed process metadata is rejected
    fn missing_duplicate_or_malformed_process_metadata_is_rejected() {
        assert!(Credentials::parse(&status(2).replace("CapAmb:", "Other:")).is_err());
        assert!(Credentials::parse(&(status(2) + "Uid:\t1000 1000 1000 1000\n")).is_err());
        assert!(Credentials::parse(&status(2).replace("Tgid:\t2", "Tgid:\t3")).is_err());
        assert!(
            Credentials::parse(&status(2).replace("CapPrm:\t0000", "CapPrm:\tbroken")).is_err()
        );
    }
    #[test]
    // Verify that process generation handles parentheses and rejects dead or recycled PID
    fn process_generation_handles_parentheses_and_rejects_dead_or_recycled_pid() {
        let stat = format!("2 (scratch ) editor) S {}1234", "0 ".repeat(18));
        assert_eq!(process_generation(&stat, 2).unwrap(), 1234);
        assert!(process_generation(&stat, 3).is_err());
        assert!(process_generation(&stat.replace(") S ", ") Z "), 2).is_err());
        assert!(process_generation("2 (short) S 1", 2).is_err());
    }
    #[test]
    // Verify that focus requires bounded noncyclic ancestry not root or pointer root
    fn focus_requires_bounded_noncyclic_ancestry_not_root_or_pointer_root() {
        assert!(valid_chain(&[30, 20, 10], 10, 2));
        assert!(valid_chain(&[10], 10, 2));
        for chain in [
            vec![],
            vec![30, 20],
            vec![1, 10],
            vec![2, 10],
            vec![30, 30, 10],
        ] {
            assert!(!valid_chain(&chain, 10, 2));
        }
        assert!(!valid_chain(&(10..43).rev().collect::<Vec<_>>(), 10, 2));
    }
    #[test]
    // Verify that confirmation may leave focus but destruction and playback focus loss are sticky
    fn confirmation_may_leave_focus_but_destruction_and_playback_focus_loss_are_sticky() {
        let mut state = Lifetime::default();
        state.focus_changed();
        state.check().unwrap();
        state.focus_armed = true;
        state.focus_changed();
        assert!(state.check().is_err());
        state.focus_armed = false;
        assert!(state.check().is_err());
        let mut selected = Lifetime::default();
        selected.lost();
        assert!(selected.check().is_err());
    }
}
