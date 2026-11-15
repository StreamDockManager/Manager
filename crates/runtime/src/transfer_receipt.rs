//! Shared observation only. Timeouts reply once but retain the operation's ownership fence
use hardware_core::OutputStatus;
use std::{sync::mpsc, time::Instant};

// Poll transfer receipt for pending work
pub(crate) fn poll<E>(
    receipt: &mpsc::Receiver<OutputStatus>,
    reply: &mut Option<mpsc::Sender<Result<(), E>>>,
    deadline: Instant,
    classify: impl FnOnce(OutputStatus) -> Result<(), E>,
    disconnected: impl FnOnce() -> E,
    timeout: impl FnOnce() -> E,
) -> bool {
    // Terminal completion wins even when observed after the deadline
    let result = match receipt.try_recv() {
        Ok(status) => Some(classify(status)),
        Err(mpsc::TryRecvError::Disconnected) => Some(Err(disconnected())),
        Err(mpsc::TryRecvError::Empty) => None,
    };
    if let Some(result) = result {
        if let Some(reply) = reply.take() {
            let _ = reply.send(result);
        }
        return true;
    }
    if Instant::now() >= deadline {
        if let Some(reply) = reply.take() {
            let _ = reply.send(Err(timeout()));
        }
    }
    false
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that timeout replies once but only terminal receipt releases fence
    fn timeout_replies_once_but_only_terminal_receipt_releases_fence() {
        let (send, receive) = mpsc::channel();
        let (reply, result) = mpsc::channel();
        let mut reply = Some(reply);
        let deadline = Instant::now();
        let mut observe = || {
            poll(
                &receive,
                &mut reply,
                deadline,
                |_| Ok(()),
                || "disconnected",
                || "timeout",
            )
        };
        assert!(!observe());
        assert_eq!(result.recv().unwrap(), Err("timeout"));
        assert!(!observe());
        send.send(OutputStatus::Completed).unwrap();
        assert!(observe());
        assert!(result.try_recv().is_err());
    }
    #[test]
    // Verify that terminal receipt precedes timeout and disconnection is terminal
    fn terminal_receipt_precedes_timeout_and_disconnection_is_terminal() {
        let (send, receive) = mpsc::channel();
        let (reply, result) = mpsc::channel();
        let mut reply = Some(reply);
        send.send(OutputStatus::Completed).unwrap();
        assert!(poll(
            &receive,
            &mut reply,
            Instant::now(),
            |_| Ok(()),
            || "closed",
            || "timeout"
        ));
        assert_eq!(result.recv().unwrap(), Ok(()));
        drop(send);
        assert!(poll(
            &receive,
            &mut reply,
            Instant::now(),
            |_| Ok(()),
            || "closed",
            || "timeout"
        ));
    }
}
