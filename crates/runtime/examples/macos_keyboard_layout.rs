//! Read-only native regression probe: resolves letter/function shortcuts, never creates
//! Enigo or posts input, opens devices, changes the clipboard, or starts plugins
#[cfg(target_os = "macos")]
#[path = "../src/keyboard_layout.rs"]
mod keyboard_layout;

#[cfg(target_os = "macos")]
// Run the macOS keyboard layout example entry point
fn main() {
    use enigo::Key;
    use objc2_foundation::{NSDate, NSRunLoop};
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    assert!(objc2::MainThreadMarker::new().is_some());
    let shortcuts = [
        vec![Key::Control, Key::Shift, Key::Unicode('m')],
        vec![Key::Control, Key::Shift, Key::Unicode('d')],
        vec![Key::F9],
        vec![Key::F10],
        vec![Key::Meta, Key::Unicode('v')],
    ];
    let expected: Vec<_> = shortcuts
        .iter()
        .cloned()
        .map(|keys| keyboard_layout::resolve(keys).unwrap())
        .collect();
    assert!(expected
        .iter()
        .flatten()
        .all(|key| !matches!(key, Key::Unicode(_))));
    assert_ne!(
        expected[0][2], expected[1][2],
        "distinct letters must resolve to distinct physical keys, not an unavailable-layout fallback"
    );
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        for _ in 0..10 {
            for (keys, expected) in shortcuts.iter().zip(&expected) {
                assert_eq!(&keyboard_layout::resolve(keys.clone()).unwrap(), expected);
            }
        }
        tx.send(()).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    while rx.try_recv().is_err() {
        assert!(
            Instant::now() < deadline,
            "main-queue layout probe timed out"
        );
        objc2::rc::autoreleasepool(|_| {
            NSRunLoop::mainRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.01));
        });
    }
    worker.join().unwrap();

    // With no serviced main loop, fail within the bound instead of deadlocking
    // (including CLI hosts/shutdown). The queued work only reads the layout.
    let worker = std::thread::spawn(|| keyboard_layout::resolve(vec![Key::Unicode('m')]));
    assert!(worker.join().unwrap().unwrap_err().contains("timed out"));
    objc2::rc::autoreleasepool(|_| {
        NSRunLoop::mainRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
    });
    println!("Passed: direct/worker layout resolution, 50 shortcut lookups, bounded timeout; no input injected.");
}

#[cfg(not(target_os = "macos"))]
// Run the macOS keyboard layout example entry point
fn main() {
    println!("macOS-only keyboard layout probe; not qualified on this platform.");
}
