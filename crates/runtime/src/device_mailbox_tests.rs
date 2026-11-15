use super::*;

// Construct the image fixture for device mailbox tests
fn image(value: u8, bytes: usize) -> Request {
    Request::Command(
        "fixture".into(),
        1,
        DeviceCommand::Image {
            control_id: "key-1".into(),
            data: vec![value; bytes],
        },
    )
}
// Receive the next device mailbox tests message
fn receive(receiver: &Receiver) -> Received {
    receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("queued request")
}

#[test]
// Verify that selected requests remain charged and admitted commands keep fifo
fn selected_requests_remain_charged_and_admitted_commands_keep_fifo() {
    let (sender, receiver) = channel();
    let mut accepted = 0;
    while sender.send(image(accepted as u8, 1024 * 1024)).is_ok() {
        accepted += 1;
    }
    assert_eq!(accepted, 15);
    let held: Vec<_> = receiver.try_iter().take(4).collect();
    let stats = sender.stats();
    assert_eq!(stats.pending_count + stats.in_flight_count, accepted);
    assert!(stats.pending_bytes + stats.in_flight_bytes <= MAX_BYTES);
    assert_eq!(stats.in_flight_count, 4);
    assert!(sender.send(image(99, 1024 * 1024)).is_err());
    drop(held);
    sender.send(image(99, 1024 * 1024)).unwrap();
    let mut values = vec![];
    for mut selected in receiver.try_iter() {
        let Request::Command(device, epoch, DeviceCommand::Image { data, .. }) =
            selected.request.take().unwrap()
        else {
            panic!("image")
        };
        assert_eq!(device, "fixture");
        assert_eq!(epoch, 1);
        values.push(data[0]);
    }
    assert_eq!(values, (4..15).chain([99]).collect::<Vec<_>>());
    assert_eq!(sender.stats().in_flight_bytes, 0);
    assert_eq!(sender.stats().pending_bytes, 0);
}

#[test]
// Verify that vector spare capacity and selected request count are bounded
fn vector_spare_capacity_and_selected_request_count_are_bounded() {
    let (sender, receiver) = channel();
    let data = Vec::with_capacity(MAX_BYTES);
    assert!(sender
        .send(Request::Command(
            "fixture".into(),
            1,
            DeviceCommand::Background {
                data,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }
        ))
        .is_err());
    for _ in 0..MAX_REQUESTS {
        sender.send(image(0, 1)).unwrap();
    }
    let held: Vec<_> = receiver.try_iter().collect();
    assert_eq!(held.len(), MAX_REQUESTS);
    assert!(sender.send(image(0, 1)).is_err());
    drop(held);
    sender.send(image(0, 1)).unwrap();
}

#[test]
// Verify that lifecycle floods use live identity slots and keep latest snapshot order
fn lifecycle_floods_use_live_identity_slots_and_keep_latest_snapshot_order() {
    let (sender, receiver) = channel();
    let registration = sender.register("fixture", 1);
    for _ in 0..10_000 {
        sender.send(Request::Wake).unwrap();
        sender.send(Request::Restore).unwrap();
        sender
            .send(Request::RestoreDevice("fixture".into(), 1))
            .unwrap();
        sender.send(Request::Lost("fixture".into(), 1)).unwrap();
    }
    assert_eq!(sender.stats().pending_count, 4);
    assert_eq!(sender.stats().lifecycle_identities, 1);
    assert_eq!(sender.stats().coalesced, 39_996);
    sender.send(image(10, 1)).unwrap();
    sender.send(Request::Restore).unwrap();
    let mut kinds = vec![];
    for mut selected in receiver.try_iter() {
        kinds.push(match selected.request.take().unwrap() {
            Request::Wake => "wake",
            Request::Restore => "restore",
            Request::RestoreDevice(_, 1) => "device",
            Request::Lost(_, 1) => "lost",
            Request::Command(_, 1, _) => "image",
            _ => panic!("unexpected request"),
        });
    }
    assert_eq!(
        kinds,
        ["wake", "restore", "device", "lost", "image", "restore"]
    );
    // Old notifications are retired on failed open/disconnect; later old-epoch
    // sends do not allocate slots or contaminate the successor's identity
    sender
        .send(Request::RestoreDevice("fixture".into(), 1))
        .unwrap();
    drop(registration);
    let _successor = sender.register("fixture", 2);
    sender.send(Request::Lost("fixture".into(), 1)).unwrap();
    assert_eq!(sender.stats().pending_count, 0);
    assert_eq!(sender.stats().lifecycle_identities, 1);
}

#[test]
// Verify that saturated visual queue preserves control replies and shutdown
fn saturated_visual_queue_preserves_control_replies_and_shutdown() {
    let (sender, receiver) = channel();
    for _ in 0..MAX_REQUESTS {
        sender.send(image(0, 1)).unwrap();
    }
    let (reply, stopped) = mpsc::channel();
    sender
        .send(Request::BenchmarkStop {
            device: "fixture".into(),
            epoch: 1,
            run_id: 2,
            reply,
        })
        .unwrap();
    let (reply, shutdown) = mpsc::channel();
    sender.send(Request::Shutdown(reply)).unwrap();
    assert!(sender.send(image(0, 1)).is_err());
    let mut selected = receiver.take_shutdown().expect("reserved shutdown slot");
    let Request::Shutdown(reply) = selected.request.take().unwrap() else {
        panic!("shutdown")
    };
    reply.send(()).unwrap();
    shutdown.recv_timeout(Duration::from_secs(1)).unwrap();
    drop(receiver);
    assert!(stopped
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap_err()
        .contains("stopped"));
    drop(selected);
    assert_eq!(sender.stats().pending_bytes, 0);
    assert_eq!(sender.stats().in_flight_bytes, 0);
}

#[test]
// Verify that rejected and retired reply requests fail explicitly
fn rejected_and_retired_reply_requests_fail_explicitly() {
    let (sender, receiver) = channel();
    let (reply, response) = mpsc::channel();
    assert!(sender
        .send(Request::BenchmarkFrame {
            device: "fixture".into(),
            epoch: 1,
            run_id: 1,
            commands: vec![DeviceCommand::Background {
                data: vec![1; MAX_BYTES],
                x: 0,
                y: 0,
                width: 1,
                height: 1
            }],
            reply,
        })
        .is_err());
    assert!(response
        .recv()
        .unwrap()
        .unwrap_err()
        .contains("queue is full"));
    let (reply, response) = mpsc::channel();
    sender
        .send(Request::BenchmarkStats {
            device: "fixture".into(),
            epoch: 1,
            run_id: 1,
            reply,
        })
        .unwrap();
    let selected = receive(&receiver);
    drop(selected);
    assert!(response
        .recv()
        .unwrap()
        .unwrap_err()
        .contains("before dispatching"));
    drop(receiver);
    assert!(sender.send(Request::Wake).is_err());
}

#[test]
// Verify that stop has a separate reserve and overtakes stalled frames and stats
fn stop_has_a_separate_reserve_and_overtakes_stalled_frames_and_stats() {
    let (sender, receiver) = channel();
    for _ in 0..MAX_REQUESTS {
        sender.send(image(0, 1)).unwrap();
    }
    for _ in 0..CONTROL_REQUESTS {
        let (reply, _) = mpsc::channel();
        sender
            .send(Request::BenchmarkStats {
                device: "fixture".into(),
                epoch: 1,
                run_id: 1,
                reply,
            })
            .unwrap();
    }
    let (reply, response) = mpsc::channel();
    sender
        .send(Request::BenchmarkStop {
            device: "fixture".into(),
            epoch: 1,
            run_id: 1,
            reply,
        })
        .unwrap();
    let mut selected = receive(&receiver);
    let Request::BenchmarkStop { reply, .. } = selected.request.take().unwrap() else {
        panic!("stop must take priority")
    };
    reply.send(Ok(())).unwrap();
    assert!(response.recv().unwrap().is_ok());
    drop(selected);
    assert_eq!(sender.stats().in_flight_count, 0);
    assert_eq!(
        sender.stats().pending_count,
        MAX_REQUESTS + CONTROL_REQUESTS
    );
}

#[test]
// Verify that timeout stop retires only its unstarted benchmark
fn timeout_stop_retires_only_its_unstarted_benchmark() {
    let (sender, receiver) = channel();
    let (reply, cancelled) = mpsc::channel();
    sender
        .send(Request::BenchmarkStart {
            device: "fixture".into(),
            epoch: 1,
            run_id: 10,
            policy: Default::default(),
            encoding: Default::default(),
            reply,
        })
        .unwrap();
    let (reply, successor) = mpsc::channel();
    sender
        .send(Request::BenchmarkStart {
            device: "fixture".into(),
            epoch: 2,
            run_id: 10,
            policy: Default::default(),
            encoding: Default::default(),
            reply,
        })
        .unwrap();
    let (reply, _) = mpsc::channel();
    sender
        .send(Request::BenchmarkStop {
            device: "fixture".into(),
            epoch: 1,
            run_id: 10,
            reply,
        })
        .unwrap();
    let mut stop = receive(&receiver);
    assert!(matches!(
        stop.request.take(),
        Some(Request::BenchmarkStop { epoch: 1, .. })
    ));
    assert!(cancelled
        .recv()
        .unwrap()
        .unwrap_err()
        .contains("cancelled before dispatch"));
    let mut next = receive(&receiver);
    let Some(Request::BenchmarkStart {
        epoch: 2, reply, ..
    }) = next.request.take()
    else {
        panic!("successor remains admitted")
    };
    reply.send(Ok(())).unwrap();
    assert!(successor.recv().unwrap().is_ok());
    assert!(receiver.try_iter().next().is_none());
    drop((next, stop));
    assert_eq!(sender.stats().in_flight_bytes, 0);
}

#[test]
// Verify that restore barriers are not coalesced across an admitted start
fn restore_barriers_are_not_coalesced_across_an_admitted_start() {
    let (sender, receiver) = channel();
    sender.send(Request::Restore).unwrap();
    let (reply, _) = mpsc::channel();
    sender
        .send(Request::BenchmarkStart {
            device: "fixture".into(),
            epoch: 1,
            run_id: 10,
            policy: Default::default(),
            encoding: Default::default(),
            reply,
        })
        .unwrap();
    sender.send(Request::Restore).unwrap();
    let mut before = receive(&receiver);
    assert!(matches!(before.request.take(), Some(Request::Restore)));
    let mut start = receive(&receiver);
    assert!(matches!(
        start.request.take(),
        Some(Request::BenchmarkStart { .. })
    ));
    let mut after = receive(&receiver);
    assert!(matches!(after.request.take(), Some(Request::Restore)));
}

#[test]
// Verify that failed command admission keeps its preceding restore pending
fn failed_command_admission_keeps_its_preceding_restore_pending() {
    let (sender, receiver) = channel();
    for _ in 0..MAX_REQUESTS {
        sender.send(image(0, 1)).unwrap();
    }
    sender.send(Request::Restore).unwrap();
    let (reply, response) = mpsc::channel();
    assert!(sender
        .send(Request::BenchmarkStart {
            device: "fixture".into(),
            epoch: 1,
            run_id: 10,
            policy: Default::default(),
            encoding: Default::default(),
            reply,
        })
        .is_err());
    assert!(response.recv().unwrap().is_err());
    assert_eq!(sender.stats().pending_count, MAX_REQUESTS + 1);
    let mut last_was_restore = false;
    for mut selected in receiver.try_iter() {
        last_was_restore = matches!(selected.request.take(), Some(Request::Restore));
    }
    assert!(
        last_was_restore,
        "rejection must not consume lifecycle state"
    );
}

#[test]
// Verify that producers wake a waiting actor and last sender disconnects
fn producers_wake_a_waiting_actor_and_last_sender_disconnects() {
    let (sender, receiver) = channel();
    let waiter = std::thread::spawn(move || {
        let mut selected = receive(&receiver);
        assert!(matches!(selected.request.take(), Some(Request::Restore)));
        drop(selected);
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    });
    sender.send(Request::Restore).unwrap();
    drop(sender);
    waiter.join().unwrap();
}

#[test]
// Verify that stalled consumer repeated bursts plateau
fn stalled_consumer_repeated_bursts_plateau() {
    let mut runs = vec![];
    for repetition in 0..5 {
        // Source-equivalent control for the former unbounded actor channel
        // This records owned request bytes, not process RSS or allocator pages
        let (legacy_sender, legacy_receiver) = mpsc::channel();
        for _ in 0..MAX_REQUESTS {
            legacy_sender.send(image(1, 32 * 1024)).unwrap();
        }
        let legacy_selected: Vec<_> = legacy_receiver.try_iter().take(64).collect();
        for _ in 0..1000 {
            legacy_sender.send(image(2, 32 * 1024)).unwrap();
            legacy_sender
                .send(Request::RestoreDevice("fixture".into(), 1))
                .unwrap();
        }
        let legacy_pending: Vec<_> = legacy_receiver.try_iter().collect();
        let legacy_count = legacy_pending.len() + legacy_selected.len();
        let legacy_bytes: usize = legacy_pending
            .iter()
            .chain(&legacy_selected)
            .map(request_bytes)
            .sum();
        drop((
            legacy_pending,
            legacy_selected,
            legacy_sender,
            legacy_receiver,
        ));
        let (sender, receiver) = channel();
        let _registration = sender.register("fixture", 1);
        for _ in 0..MAX_REQUESTS {
            sender.send(image(1, 32 * 1024)).unwrap();
        }
        let held: Vec<_> = receiver.try_iter().take(64).collect();
        let before = sender.stats();
        for _ in 0..1000 {
            assert!(sender.send(image(2, 32 * 1024)).is_err());
            sender
                .send(Request::RestoreDevice("fixture".into(), 1))
                .unwrap();
        }
        let peak = sender.stats();
        assert!(legacy_bytes > peak.pending_bytes + peak.in_flight_bytes);
        assert_eq!(
            peak.pending_bytes + peak.in_flight_bytes,
            before.pending_bytes + before.in_flight_bytes + size_of::<Request>() + "fixture".len()
        );
        drop(held);
        drop(receiver);
        let after = sender.stats();
        assert_eq!(after.pending_bytes + after.in_flight_bytes, 0);
        runs.push(serde_json::json!({"repetition": repetition, "legacy_admitted_requests": legacy_count, "legacy_admitted_bytes": legacy_bytes, "accepted_requests": MAX_REQUESTS, "selected_requests": 64, "burst_rejections": peak.rejected, "coalesced_lifecycle_updates": peak.coalesced, "peak_admitted_bytes": peak.pending_bytes + peak.in_flight_bytes, "after_close_admitted_bytes": 0}));
    }
    eprintln!(
        "DEVICE_MAILBOX_PLATEAU={}",
        serde_json::to_string(&runs).unwrap()
    );
}
