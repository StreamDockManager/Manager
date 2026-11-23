//! Readiness-driven accept and worker completion. No host locks cross poll/join.
use mio::{Events, Interest, Poll, Token, Waker};
use std::{
    collections::HashMap,
    io,
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
};
const SOCKET: Token = Token(0);
const WAKE: Token = Token(1);
const BATCH: usize = 64;

pub(super) struct Completion {
    id: u64,
    completed: mpsc::Sender<u64>,
    wake: Arc<Waker>,
}
impl Drop for Completion {
    fn drop(&mut self) {
        let _ = self.completed.send(self.id);
        let _ = self.wake.wake();
    }
}
#[cfg(test)]
#[derive(Default)]
struct Stats {
    polls: std::sync::atomic::AtomicUsize,
    reaped: std::sync::atomic::AtomicUsize,
}
pub(super) struct Listener {
    #[cfg(test)]
    stats: Arc<Stats>,
    socket: mio::net::TcpListener,
    poll: Poll,
    pub wake: Arc<Waker>,
}
impl Listener {
    pub fn new(socket: TcpListener) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let mut socket = mio::net::TcpListener::from_std(socket);
        let poll = Poll::new()?;
        poll.registry()
            .register(&mut socket, SOCKET, Interest::READABLE)?;
        let wake = Arc::new(Waker::new(poll.registry(), WAKE)?);
        Ok(Self {
            socket,
            poll,
            wake,
            #[cfg(test)]
            stats: Arc::default(),
        })
    }
    pub fn run(
        mut self,
        stop: Arc<AtomicBool>,
        mut spawn: impl FnMut(TcpStream, Completion) -> Option<JoinHandle<()>>,
    ) {
        let (completed, receive) = mpsc::channel();
        let mut workers = HashMap::new();
        let mut next_id = 0u64;
        let mut events = Events::with_capacity(8);
        let mut accept_ready = false;
        'listener: loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let mut reaped = 0;
            while reaped < BATCH {
                let Ok(id) = receive.try_recv() else {
                    break;
                };
                if let Some(worker) = workers.remove(&id) {
                    // Completion guard is the worker's final operation, including
                    // unwind; join never waits for socket I/O or a held host lock.
                    let _: std::thread::Result<()> = JoinHandle::join(worker);
                    #[cfg(test)]
                    self.stats.reaped.fetch_add(1, Ordering::SeqCst);
                }
                reaped += 1;
            }
            if accept_ready {
                for _ in 0..BATCH {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match self.socket.accept() {
                        Ok((stream, _)) => {
                            let Some(id) = next_id.checked_add(1) else {
                                break 'listener;
                            };
                            next_id = id;
                            let completion = Completion {
                                id,
                                completed: completed.clone(),
                                wake: self.wake.clone(),
                            };
                            if let Some(worker) = spawn(stream.into(), completion) {
                                workers.insert(id, worker);
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            accept_ready = false;
                            break;
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::ConnectionAborted | io::ErrorKind::ConnectionReset
                            ) =>
                        {
                            continue
                        }
                        Err(error) => {
                            log::error!("Plugin listener accept failed: {error}");
                            break 'listener;
                        }
                    }
                }
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            // A bounded batch must not strand edge-triggered readiness (including
            // Windows): continue draining locally until WouldBlock, fairly reaping
            // completions and checking shutdown between batches.
            if accept_ready || reaped == BATCH {
                continue;
            }
            #[cfg(test)]
            self.stats.polls.fetch_add(1, Ordering::SeqCst);
            match self.poll.poll(&mut events, None) {
                Ok(()) => {
                    accept_ready = events.iter().any(|event| event.token() == SOCKET);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    log::error!("Plugin listener readiness failed: {error}");
                    break;
                }
            }
        }
        for (_, worker) in workers {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn idle_accept_completion_and_shutdown_are_signaled() {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        let listener = Listener::new(socket).unwrap();
        let wake = listener.wake.clone();
        let stats = listener.stats.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (sent, received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            listener.run(stopped, move |stream, completion| {
                let sent = sent.clone();
                Some(std::thread::spawn(move || {
                    let _completion = completion;
                    drop(stream);
                    sent.send(()).unwrap();
                }))
            })
        });
        assert!(received.recv_timeout(Duration::from_millis(120)).is_err());
        assert_eq!(stats.polls.load(Ordering::SeqCst), 1);
        assert!(received.recv_timeout(Duration::from_millis(120)).is_err());
        assert_eq!(
            stats.polls.load(Ordering::SeqCst),
            1,
            "idle listener must remain in a single indefinite poll"
        );
        for _ in 0..(BATCH * 3) {
            drop(TcpStream::connect(address).unwrap());
        }
        for _ in 0..(BATCH * 3) {
            received.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while stats.reaped.load(Ordering::SeqCst) < BATCH * 3
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            stats.reaped.load(Ordering::SeqCst),
            BATCH * 3,
            "completed handles must be joined with no further connection"
        );
        stop.store(true, Ordering::SeqCst);
        wake.wake().unwrap();
        worker.join().unwrap();
    }
}
