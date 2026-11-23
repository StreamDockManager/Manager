use super::*;
use crate::clipboard::{capture_text, copy_result, MAX_TEXT_BYTES};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn wayland_peer(program: &str) -> Command {
    let mut command = Command::new(program);
    if let Some(seat) = std::env::var_os("XDG_SEAT") {
        command.arg("--seat").arg(seat);
    }
    command
}

// Run only against a private X server or compositor. The peer is independent of Manager.
fn publish(session: Session, mime: &str, bytes: &[u8]) {
    use x11rb::protocol::xproto::ConnectionExt;
    let x_display = (session == Session::X11).then(|| {
        let (conn, _) = x11rb::connect(None).unwrap();
        let atom = conn
            .intern_atom(false, b"CLIPBOARD")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        let before = conn
            .get_selection_owner(atom)
            .unwrap()
            .reply()
            .unwrap()
            .owner;
        (conn, atom, before)
    });
    let mut cmd = match session {
        Session::X11 => {
            let mut cmd = Command::new("xclip");
            cmd.args(["-quiet", "-selection", "clipboard", "-in", "-target", mime]);
            cmd
        }
        Session::Wayland => {
            let mut cmd = wayland_peer("wl-copy");
            cmd.args(["--type", mime]);
            cmd
        }
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    if let Some((conn, atom, before)) = x_display {
        // xclip's default background fork returns before the X server acknowledges ownership.
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let owner = conn
                .get_selection_owner(atom)
                .unwrap()
                .reply()
                .unwrap()
                .owner;
            if owner != x11rb::NONE && owner != before {
                break;
            }
            assert!(child.try_wait().unwrap().is_none() && Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    } else {
        assert!(child.wait().unwrap().success());
    }
}

fn paste(session: Session) -> Vec<u8> {
    let out = match session {
        Session::X11 => Command::new("xclip")
            .args(["-selection", "clipboard", "-out", "-target", "UTF8_STRING"])
            .output(),
        Session::Wayland => wayland_peer("wl-paste")
            .args(["--no-newline", "--type", "text/plain;charset=utf-8"])
            .output(),
    }
    .unwrap();
    assert!(out.status.success());
    out.stdout
}

#[test]
#[ignore = "requires SDM_DISPOSABLE_CLIPBOARD=1 and an isolated X11 or Wayland display"]
fn independent_client_reads_writes_replaces_and_preserves_rejected_results() {
    assert_eq!(
        std::env::var("SDM_DISPOSABLE_CLIPBOARD").as_deref(),
        Ok("1")
    );
    let session = session().unwrap();
    // A bare Xvfb resets the server when its last client exits. A desktop normally
    // has a window manager keeping that connection alive, even with an empty selection.
    let _display = (session == Session::X11).then(|| x11rb::connect(None).unwrap());
    let mime = match session {
        Session::X11 => "UTF8_STRING",
        Session::Wayland => "text/plain;charset=utf-8",
    };
    assert!(available());
    match session {
        Session::Wayland => assert!(wayland_peer("wl-copy")
            .arg("--clear")
            .status()
            .unwrap()
            .success()),
        Session::X11 => {
            use x11rb::protocol::xproto::ConnectionExt;
            let (conn, _) = x11rb::connect(None).unwrap();
            let atom = conn
                .intern_atom(false, b"CLIPBOARD")
                .unwrap()
                .reply()
                .unwrap()
                .atom;
            conn.set_selection_owner(x11rb::NONE, atom, x11rb::CURRENT_TIME)
                .unwrap()
                .check()
                .unwrap();
        }
    }
    assert_eq!(capture_text(&|| false), Err(ClipboardError::NoText));
    let limit = "😺".repeat(MAX_TEXT_BYTES / 4);
    for text in ["", "line one\nα😺\n", &limit] {
        publish(session, mime, text.as_bytes());
        assert_eq!(capture_text(&|| false).unwrap().as_str(), text);
        // Replace the peer, then paste twice after Copy has returned.
        copy_result(text).unwrap();
        assert_eq!(paste(session), text.as_bytes());
        assert_eq!(paste(session), text.as_bytes());
    }
    assert_eq!(
        copy_result(&format!("{limit}x")),
        Err(ClipboardError::TooLarge)
    );
    assert_eq!(paste(session), limit.as_bytes());
    publish(session, mime, format!("{limit}x").as_bytes());
    assert_eq!(capture_text(&|| false), Err(ClipboardError::TooLarge));
    publish(session, mime, &[0xff]);
    assert_eq!(
        capture_text(&|| false),
        Err(ClipboardError::InvalidEncoding)
    );
    publish(session, mime, b"first owner");
    let checks = std::cell::Cell::new(0);
    let changed = capture_text(&|| {
        checks.set(checks.get() + 1);
        if checks.get() == 2 {
            publish(session, mime, b"replacement owner");
        }
        false
    });
    assert_eq!(changed, Err(ClipboardError::Changed));
    assert_eq!(
        capture_text(&|| false).unwrap().as_str(),
        "replacement owner"
    );
    let checked = std::cell::Cell::new(false);
    assert_eq!(
        capture_text(&|| checked.replace(true)),
        Err(ClipboardError::Cancelled)
    );
    publish(session, "image/png", b"fixture");
    assert_eq!(
        capture_text(&|| false),
        Err(ClipboardError::UnsupportedFormat)
    );
    assert_eq!(capture_text(&|| true), Err(ClipboardError::Cancelled));
}

#[test]
#[ignore = "requires an unreachable Wayland display and an independently accessible X display"]
fn unavailable_wayland_never_borrows_x11() {
    assert_eq!(
        std::env::var("SDM_DISPOSABLE_CLIPBOARD").as_deref(),
        Ok("1")
    );
    assert!(x11::available());
    assert!(matches!(session(), Ok(Session::Wayland)));
    assert!(!available());
    assert_eq!(
        capture_text(&|| false),
        Err(ClipboardError::UnsupportedBackend)
    );
    assert_eq!(
        copy_result("fixture"),
        Err(ClipboardError::UnsupportedBackend)
    );
}
