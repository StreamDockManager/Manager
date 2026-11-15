//! Readiness-driven sockets with producer wakeups and bounded writes
use super::*;
use mio::{Events, Interest, Poll, Token, Waker};
use std::time::Instant;

const SOCKET: Token = Token(0);
const QUEUE: Token = Token(1);

pub(super) const OUTGOING_BYTES: usize = 8 * 1024 * 1024;
pub(super) const OUTGOING_MESSAGES: usize = 256;
const BATCH_MESSAGES: usize = 32;
const BATCH_BYTES: usize = MAX_MESSAGE_BYTES;

struct Queued {
    value: Value,
    bytes: usize,
}
#[derive(Default)]
struct SelectedUsage {
    bytes: std::sync::atomic::AtomicUsize,
    messages: std::sync::atomic::AtomicUsize,
}
pub(super) struct Selected {
    pub value: Value,
    bytes: usize,
    usage: Arc<SelectedUsage>,
}
impl Drop for Selected {
    // Release resources held by this owner
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.value));
        self.usage.bytes.fetch_sub(self.bytes, Ordering::SeqCst);
        self.usage.messages.fetch_sub(1, Ordering::SeqCst);
    }
}

// Count encoded UTF-8 without allocating a second copy of each queued Value
// Measure serialized JSON bytes without allocating a second payload
fn message_bytes(value: &Value) -> Result<usize, &'static str> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        // Count serialized bytes with overflow protection
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_MESSAGE_BYTES - self.0 {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        // Flush for the current operation
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter(0);
    serde_json::to_writer(&mut count, value).map_err(|_| "plugin message exceeds 1 MiB")?;
    Ok(count.0)
}

#[derive(Default)]
pub(super) struct Outgoing {
    messages: VecDeque<Queued>,
    pending_bytes: usize,
    selected: Arc<SelectedUsage>,
    failure: Option<&'static str>,
    pub wake: Option<Arc<Waker>>,
    pub theme: Option<Value>,
    pub display_theme: Option<Value>,
}
impl Outgoing {
    // Notify for the current operation
    fn notify(&self) {
        if let Some(wake) = &self.wake {
            let _ = wake.wake();
        }
    }
    // Push back
    pub fn push_back(&mut self, value: Value) {
        if let Err(error) = self.push_batch(vec![value]) {
            self.fail(error);
        }
    }
    // Push front
    pub fn push_front(&mut self, value: Value) {
        match self.admit(vec![value]) {
            Ok(mut messages) => {
                let message = messages.pop().unwrap();
                self.pending_bytes += message.bytes;
                self.messages.push_front(message);
            }
            Err(error) => self.fail(error),
        }
        self.notify();
    }
    // Check the entire message batch against both count and byte budgets
    fn admit(&self, values: Vec<Value>) -> Result<Vec<Queued>, &'static str> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if values.len() > OUTGOING_MESSAGES.saturating_sub(self.retained_messages()) {
            return Err("plugin outbound queue full (256 messages)");
        }
        let mut available = OUTGOING_BYTES.saturating_sub(self.retained_bytes());
        let mut messages = Vec::with_capacity(values.len());
        for value in values {
            let bytes = message_bytes(&value)?;
            if bytes > available {
                return Err("plugin outbound queue full (8 MiB)");
            }
            available -= bytes;
            messages.push(Queued { value, bytes });
        }
        Ok(messages)
    }
    // Commands use atomic admission so a pulse cannot enqueue its press without
    // its release. Other producers fail the connection explicitly on overflow.
    pub fn push_batch(&mut self, values: Vec<Value>) -> Result<(), &'static str> {
        let messages = self.admit(values)?;
        for message in messages {
            self.pending_bytes += message.bytes;
            self.messages.push_back(message);
        }
        self.notify();
        Ok(())
    }
    // Record the failure of the pending operation
    pub fn fail(&mut self, error: &'static str) {
        self.failure = Some(error);
        self.clear();
        self.notify();
    }
    // Read the byte count retained by the transport
    pub fn retained_bytes(&self) -> usize {
        self.pending_bytes + self.selected.bytes.load(Ordering::SeqCst)
    }
    // Read the message count retained by the transport
    pub fn retained_messages(&self) -> usize {
        self.messages.len() + self.selected.messages.load(Ordering::SeqCst)
    }
    // Take batch
    pub fn take_batch(&mut self) -> Result<Vec<Selected>, &'static str> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let mut result = Vec::with_capacity(BATCH_MESSAGES.min(self.messages.len()));
        let mut bytes = 0;
        while let Some(message) = self.messages.front() {
            if result.len() >= BATCH_MESSAGES || message.bytes > BATCH_BYTES - bytes {
                break;
            }
            let message = self.messages.pop_front().unwrap();
            self.pending_bytes -= message.bytes;
            bytes += message.bytes;
            self.selected
                .bytes
                .fetch_add(message.bytes, Ordering::SeqCst);
            self.selected.messages.fetch_add(1, Ordering::SeqCst);
            result.push(Selected {
                value: message.value,
                bytes: message.bytes,
                usage: self.selected.clone(),
            });
        }
        // A bounded batch must not leave remaining work asleep after the
        // socket consumes the previous producer wakeup.
        if !self.messages.is_empty() {
            self.notify();
        }
        Ok(result)
    }
    // Retain for the current operation
    pub fn retain(&mut self, mut keep: impl FnMut(&Value) -> bool) {
        // Release the byte charge only for messages removed by the caller’s predicate
        self.messages.retain(|message| {
            let retained = keep(&message.value);
            if !retained {
                self.pending_bytes -= message.bytes;
            }
            retained
        });
    }
    // Clear for the current operation
    pub fn clear(&mut self) {
        self.messages.clear();
        self.pending_bytes = 0;
    }
    #[cfg(test)]
    // Iterate over the retained entries
    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.messages.iter().map(|message| &message.value)
    }
    #[cfg(test)]
    // Return the number of entries
    pub fn len(&self) -> usize {
        self.messages.len()
    }
    #[cfg(test)]
    // Check whether the value is empty
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
    #[cfg(test)]
    // Pop front
    pub fn pop_front(&mut self) -> Option<Value> {
        self.messages.pop_front().map(|message| {
            self.pending_bytes -= message.bytes;
            message.value
        })
    }
}
impl Drop for Outgoing {
    // Release resources held by this owner
    fn drop(&mut self) {
        self.notify();
    }
}

pub(super) struct Registration {
    pub owner: String,
    pub token: String,
    pub context: Option<String>,
}

/// The caller holds the hub lock through validation, wake ownership and initial
/// messages. Rejected connections never acquire a session to retire on exit.
pub(super) fn register(hub: &mut Hub, message: &Value, wake: Arc<Waker>) -> Option<Registration> {
    let token = message["uuid"].as_str()?;
    let owner = hub.sessions.tokens.get(token)?.clone();
    let context = hub
        .sessions
        .files
        .get(token)
        .and_then(|grant| grant.context.clone());
    let inspector = context.is_some();
    let expected = if inspector {
        "registerPropertyInspector"
    } else {
        "registerPlugin"
    };
    if message["event"] != expected {
        return None;
    }
    if inspector {
        if hub.configuration_sessions.contains(token) {
            return None;
        }
    } else {
        if let Some(capabilities) = hub.capabilities.get(&owner) {
            if serde_json::to_value(capabilities).ok().as_ref()
                != Some(&message["payload"]["capabilities"])
            {
                return None;
            }
        }
        if !hub.sessions.connected.insert(token.into()) {
            return None;
        }
        if let Some(logs) = &hub.logs {
            logs.record(
                &owner,
                hub.run_details
                    .get(&owner)
                    .and_then(|(_, run)| run.as_deref())
                    .unwrap_or(""),
                "stdout",
                "manager",
                "Plugin registered",
            );
        }
    }
    hub.sessions.storage_sessions.insert(
        token.into(),
        Arc::new(std::sync::atomic::AtomicBool::new(true)),
    );
    let key = if inspector {
        format!("pi:{token}")
    } else {
        owner.clone()
    };
    let queue = hub.sessions.queues.entry(key).or_default();
    queue.wake = Some(wake);
    queue.theme = None;
    queue.display_theme = None;
    if let Some(context) = &context {
        let settings = hub
            .context_state
            .settings
            .get(context)
            .cloned()
            .unwrap_or(json!({}));
        hub.sessions.queues.entry(format!("pi:{token}")).or_default().push_back(json!({"event":"didReceiveSettings","context":context,"payload":{"settings":settings}}));
    } else {
        if message["payload"]["network_debug"] == true {
            hub.network_debug_plugins.insert(owner.clone());
        } else {
            hub.network_debug_plugins.remove(&owner);
        }
        // Current Elgato callbacks interpret id-less globals as a PI edit, not initialization
        // Its getGlobalSettings request supplies the correlation identifier for initial state
        if hub
            .manifest_dialects
            .get(&owner)
            .is_none_or(|dialect| dialect != "streamdeck")
            || hub.capabilities.contains_key(&owner)
        {
            let settings = hub.globals.get(&owner).cloned().unwrap_or(json!({}));
            hub.sessions
                .queues
                .entry(owner.clone())
                .or_default()
                .push_front(
                    json!({"event":"didReceiveGlobalSettings","payload":{"settings":settings}}),
                );
        }
        hub.queue_log_preferences(&owner);
    }
    hub.refresh_log_secrets();
    hub.queue_theme(&owner, token, inspector);
    if inspector {
        hub.configuration_sessions.insert(token.into());
    }
    hub.announce_configuration_inspectors(&owner);
    Some(Registration {
        owner,
        token: token.into(),
        context,
    })
}

pub(super) struct Socket {
    ws: tungstenite::WebSocket<TcpStream>,
    source: mio::net::TcpStream,
    poll: Poll,
    events: Events,
    pub wake: Arc<Waker>,
    registration_deadline: Option<Instant>,
}
impl Socket {
    // Construct the value and initialize its state
    pub fn new(ws: tungstenite::WebSocket<TcpStream>) -> std::io::Result<Self> {
        ws.get_ref().set_nonblocking(true)?;
        ws.get_ref().set_read_timeout(None)?;
        ws.get_ref().set_write_timeout(None)?;
        let mut source = mio::net::TcpStream::from_std(ws.get_ref().try_clone()?);
        let poll = Poll::new()?;
        poll.registry()
            .register(&mut source, SOCKET, Interest::READABLE)?;
        let wake = Arc::new(Waker::new(poll.registry(), QUEUE)?);
        Ok(Self {
            ws,
            source,
            poll,
            wake,
            events: Events::with_capacity(4),
            registration_deadline: Some(Instant::now() + Duration::from_secs(5)),
        })
    }
    // The websocket retains handshake-buffered bytes on its std socket. All
    // operations must still pass through Mio's try_io so Windows readiness is
    // rearmed after WouldBlock on the monitored socket clone.
    fn io<T>(
        &mut self,
        operation: impl FnOnce(&mut tungstenite::WebSocket<TcpStream>) -> tungstenite::Result<T>,
    ) -> tungstenite::Result<T> {
        self.source
            .try_io(|| match operation(&mut self.ws) {
                Err(tungstenite::Error::Io(error)) => Err(error),
                result => Ok(result),
            })
            .map_err(tungstenite::Error::Io)?
    }
    // Clear the registration deadline once the plugin has registered
    pub fn registered(&mut self) {
        self.registration_deadline = None;
    }
    // Read for the current operation
    pub fn read(&mut self) -> tungstenite::Result<tungstenite::Message> {
        match self.io(|ws| ws.read()) {
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                self.flush_pending()?;
                let timeout = self
                    .registration_deadline
                    .map(|deadline| deadline.saturating_duration_since(Instant::now()));
                self.poll.poll(&mut self.events, timeout)?;
                // Always return to the owner loop, which checks queue, token,
                // shutdown and registration deadline before reading again.
                Err(tungstenite::Error::Io(
                    std::io::ErrorKind::WouldBlock.into(),
                ))
            }
            result => result,
        }
    }
    // Send for the current operation
    pub fn send(&mut self, message: tungstenite::Message) -> tungstenite::Result<()> {
        if message.len() > MAX_MESSAGE_BYTES {
            return Err(tungstenite::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Plugin message exceeds 1048576 UTF-8 bytes",
            )));
        }
        match self.io(|ws| ws.send(message)) {
            Ok(()) => return Ok(()),
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        self.flush_pending()
    }
    // Flush pending
    fn flush_pending(&mut self) -> tungstenite::Result<()> {
        match self.io(|ws| ws.flush()) {
            Ok(()) => return Ok(()),
            Err(tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        // WouldBlock means tungstenite owns the accepted message. Flush it;
        // resending would duplicate output. A slow peer has one bounded second.
        let deadline = Instant::now() + Duration::from_secs(1);
        self.poll.registry().reregister(
            &mut self.source,
            SOCKET,
            Interest::READABLE.add(Interest::WRITABLE),
        )?;
        let result = loop {
            if Instant::now() >= deadline {
                break Err(tungstenite::Error::Io(std::io::ErrorKind::TimedOut.into()));
            }
            self.poll.poll(
                &mut self.events,
                Some(deadline.saturating_duration_since(Instant::now())),
            )?;
            match self.io(|ws| ws.flush()) {
                Ok(()) => break Ok(()),
                Err(tungstenite::Error::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => break Err(error),
            }
        };
        self.poll
            .registry()
            .reregister(&mut self.source, SOCKET, Interest::READABLE)?;
        result
    }
    // Close for the current operation
    pub fn close(
        &mut self,
        frame: Option<tungstenite::protocol::CloseFrame>,
    ) -> tungstenite::Result<()> {
        self.io(|ws| ws.close(frame))
    }
}
