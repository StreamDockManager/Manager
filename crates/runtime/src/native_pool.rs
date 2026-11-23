//! Blocking native leaves use eight workers. Root lanes and waits remain in the scheduler
use std::sync::{mpsc, Arc, Mutex, OnceLock};
type Job = Box<dyn FnOnce() + Send + 'static>;
// Initialize an eight-worker native pool with a bounded 128-job submission queue
fn sender() -> &'static mpsc::SyncSender<Job> {
    static POOL: OnceLock<mpsc::SyncSender<Job>> = OnceLock::new();
    POOL.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<Job>(128);
        let receiver = Arc::new(Mutex::new(receiver));
        for _ in 0..8 {
            let receiver = receiver.clone();
            std::thread::spawn(move || loop {
                let job = receiver.lock().unwrap().recv();
                match job {
                    Ok(job) => job(),
                    Err(_) => break,
                }
            });
        }
        sender
    })
}
// Drive the native pool operation
pub fn run<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
    check: impl Fn() -> Result<(), String>,
    owned_cancellation: impl Fn() -> bool,
) -> Result<T, String> {
    check()?;
    let (reply, result) = mpsc::sync_channel(1);
    sender()
        .try_send(Box::new(move || {
            let _ = reply.send(work());
        }))
        .map_err(|_| "Native action queue is full")?;
    loop {
        if let Err(cancelled) = check() {
            // The worker may still own a pressed key, mouse button, or child process.
            // Keep the owning lane until its scoped cleanup completes.
            if owned_cancellation() {
                let _ = result.recv();
            }
            return Err(cancelled);
        }
        match result.recv_timeout(std::time::Duration::from_millis(10)) {
            Ok(value) => {
                check()?;
                return value;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err("Native action worker stopped".into()),
        }
    }
}
