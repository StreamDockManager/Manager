//! Display-dependent regression for the WebView -> IPC callback ownership cycle
#![cfg(target_os = "linux")]

use gtk::prelude::*;
use std::{
    cell::Cell,
    rc::Rc,
    thread,
    time::{Duration, Instant},
};
use wry::{WebContext, WebViewBuilder, WebViewBuilderExtUnix};

// Wait until
fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "WebKit lifetime/IPC condition timed out"
        );
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires a Linux GTK display and WebKitGTK helpers; run under xvfb-run"]
// Verify IPC works after reopen and releases its capture on close
fn ipc_works_after_reopen_and_releases_its_capture_on_close() {
    gtk::init().expect("GTK display");
    // Match Tauri's retained context: closing a window must not require dropping it
    let mut context = WebContext::new(None);
    for _ in 0..2 {
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.add(&container);
        let received = Rc::new(Cell::new(false));
        let marker = Rc::new(());
        let marker_weak = Rc::downgrade(&marker);
        let callback_received = Rc::clone(&received);
        let view = WebViewBuilder::new_with_web_context(&mut context)
            .with_ipc_handler(move |request| {
                let _keep_until_callback_is_freed = &marker;
                assert_eq!(request.body(), "lifetime-test");
                callback_received.set(true);
            })
            .with_html("<script>window.ipc.postMessage('lifetime-test')</script>")
            .build_gtk(&container)
            .expect("WebView");
        window.show_all();
        wait_until(|| received.get());
        drop(view);
        // GTK destruction is unsafe because callbacks must not access this window again
        unsafe { window.destroy() };
        drop(container);
        drop(window);
        wait_until(|| marker_weak.upgrade().is_none());
    }
}
