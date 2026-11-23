//! A transient UTF-8 clipboard snapshot. No clipboard polling or storage.
use zeroize::Zeroizing;

#[cfg(target_os = "linux")]
mod linux;

pub const MAX_TEXT_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum ClipboardError {
    NoText,
    UnsupportedFormat,
    UnsupportedBackend,
    InvalidEncoding,
    TooLarge,
    Changed,
    Access,
    Cancelled,
}

pub fn read_available() -> bool {
    #[cfg(target_os = "linux")]
    return linux::available();
    #[cfg(not(target_os = "linux"))]
    cfg!(any(target_os = "macos", target_os = "windows"))
}

pub fn copy_available() -> bool {
    #[cfg(target_os = "windows")]
    return windows::ApplicationModel::DataTransfer::ClipboardContentOptions::new().is_ok();
    #[cfg(not(target_os = "windows"))]
    read_available()
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn snapshot(
    limit: usize,
    cancelled: &dyn Fn() -> bool,
    generation: impl Fn() -> i64,
    read: impl FnOnce(usize) -> Result<Vec<u8>, ClipboardError>,
) -> Result<Zeroizing<String>, ClipboardError> {
    use zeroize::Zeroize;
    if cancelled() {
        return Err(ClipboardError::Cancelled);
    }
    let before = generation();
    let bytes = read(limit).map(Zeroizing::new);
    // Check cancellation and ownership even if the old provider failed while reading.
    if cancelled() {
        return Err(ClipboardError::Cancelled);
    }
    if before != generation() {
        return Err(ClipboardError::Changed);
    }
    let mut data = bytes?;
    if data.len() > limit {
        return Err(ClipboardError::TooLarge);
    }
    String::from_utf8(std::mem::take(&mut *data))
        .map(Zeroizing::new)
        .map_err(|error| {
            error.into_bytes().zeroize();
            ClipboardError::InvalidEncoding
        })
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};

    pub(super) fn write(board: &NSPasteboard, text: &str) -> Result<(), ClipboardError> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(ClipboardError::TooLarge);
        }
        let value = objc2_foundation::NSString::from_str(text);
        board.clearContents();
        board
            .setString_forType(&value, unsafe { NSPasteboardTypeString })
            .then_some(())
            .ok_or(ClipboardError::Access)
    }

    // https://developer.apple.com/documentation/appkit/nspasteboard/data(fortype:)
    // A nil result after the type check can also mean the owner failed to provide data.
    pub(super) fn read(
        board: &NSPasteboard,
        limit: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Zeroizing<String>, ClipboardError> {
        snapshot(
            limit,
            cancelled,
            || board.changeCount() as i64,
            |limit| {
                let format = unsafe { NSPasteboardTypeString };
                let formats = board.types().ok_or(ClipboardError::Access)?;
                if formats.is_empty() {
                    return Err(ClipboardError::NoText);
                }
                if !formats.containsObject(format) {
                    return Err(ClipboardError::UnsupportedFormat);
                }
                let data = board.dataForType(format).ok_or(ClipboardError::Access)?;
                if data.length() > limit {
                    return Err(ClipboardError::TooLarge);
                }
                Ok(data.to_vec())
            },
        )
    }

    pub(super) fn current(
        limit: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Zeroizing<String>, ClipboardError> {
        objc2::rc::autoreleasepool(|_| {
            objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
                read(&NSPasteboard::generalPasteboard(), limit, cancelled)
            }))
            .map_err(|_| ClipboardError::Access)?
        })
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) fn builtin_text(limit: usize) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    use macos::current;
    #[cfg(target_os = "windows")]
    use windows_board::current;
    let mut text =
        current(limit, &|| false).map_err(|error| format!("Clipboard read failed ({error:?})"))?;
    Ok(std::mem::take(&mut *text))
}

#[cfg(any(target_os = "windows", test))]
fn decode_wide(
    data: &[u16],
    bytes_available: usize,
    limit: usize,
) -> Result<Vec<u8>, ClipboardError> {
    let max_units = limit.saturating_add(1);
    let count = data
        .iter()
        .take(max_units)
        .position(|unit| *unit == 0)
        .ok_or(if bytes_available / 2 > limit {
            ClipboardError::TooLarge
        } else {
            ClipboardError::InvalidEncoding
        })?;
    let mut result = Zeroizing::new(String::new());
    for codepoint in char::decode_utf16(data[..count].iter().copied()) {
        let codepoint = codepoint.map_err(|_| ClipboardError::InvalidEncoding)?;
        if result.len() + codepoint.len_utf8() > limit {
            return Err(ClipboardError::TooLarge);
        }
        result.push(codepoint);
    }
    Ok(std::mem::take(&mut *result).into_bytes())
}

#[cfg(target_os = "windows")]
mod windows_board {
    use super::*;
    use windows::Win32::{
        Foundation::{GetLastError, SetLastError, ERROR_SUCCESS, HGLOBAL},
        System::{
            DataExchange::{
                CloseClipboard, CountClipboardFormats, GetClipboardData,
                GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
            },
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
            Ole::CF_UNICODETEXT,
        },
    };

    // The foreground result window calls this on its UI thread. Publish the complete
    // package in one operation, without emptying the previous clipboard first.
    pub(super) fn write(text: &str) -> Result<(), ClipboardError> {
        use windows::core::HSTRING;
        use windows::ApplicationModel::DataTransfer::{
            Clipboard, ClipboardContentOptions, DataPackage,
        };
        // CF_UNICODETEXT consumers terminate at the first NUL.
        if text.contains('\0') {
            return Err(ClipboardError::InvalidEncoding);
        }
        let publish = || -> windows::core::Result<bool> {
            let data = DataPackage::new()?;
            data.SetText(&HSTRING::from(text))?;
            let options = ClipboardContentOptions::new()?;
            options.SetIsAllowedInHistory(false)?;
            options.SetIsRoamable(false)?;
            Clipboard::SetContentWithOptions(&data, &options)
        };
        if !publish().map_err(|_| ClipboardError::Access)? {
            return Err(ClipboardError::Access);
        }
        // The data is already published. Flushing lets it outlive Manager as well.
        let _ = Clipboard::Flush();
        Ok(())
    }

    struct OpenBoard;
    impl Drop for OpenBoard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }
    struct LockedData(HGLOBAL);
    impl Drop for LockedData {
        fn drop(&mut self) {
            unsafe {
                let _ = GlobalUnlock(self.0);
            }
        }
    }

    // https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getclipboarddata
    pub(super) fn current(
        limit: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Zeroizing<String>, ClipboardError> {
        if cancelled() {
            return Err(ClipboardError::Cancelled);
        }
        unsafe {
            OpenClipboard(None).map_err(|_| ClipboardError::Access)?;
            let _board = OpenBoard;
            if GetClipboardSequenceNumber() == 0 {
                return Err(ClipboardError::Access);
            }
            snapshot(
                limit,
                cancelled,
                || i64::from(GetClipboardSequenceNumber()),
                |limit| {
                    let format = u32::from(CF_UNICODETEXT.0);
                    if IsClipboardFormatAvailable(format).is_err() {
                        SetLastError(ERROR_SUCCESS);
                        return Err(match CountClipboardFormats() {
                            0 if GetLastError() == ERROR_SUCCESS => ClipboardError::NoText,
                            0 => ClipboardError::Access,
                            _ => ClipboardError::UnsupportedFormat,
                        });
                    }
                    let handle = HGLOBAL(
                        GetClipboardData(format)
                            .map_err(|_| ClipboardError::Access)?
                            .0,
                    );
                    let available = GlobalSize(handle);
                    if available < 2 {
                        return Err(ClipboardError::InvalidEncoding);
                    }
                    let ptr = GlobalLock(handle);
                    if ptr.is_null() {
                        return Err(ClipboardError::Access);
                    }
                    let _locked = LockedData(handle);
                    let units = std::slice::from_raw_parts(
                        ptr.cast::<u16>(),
                        (available / 2).min(limit + 1),
                    );
                    decode_wide(units, available, limit)
                },
            )
        }
    }
}

pub fn capture_text(cancelled: &dyn Fn() -> bool) -> Result<Zeroizing<String>, ClipboardError> {
    #[cfg(target_os = "macos")]
    {
        macos::current(MAX_TEXT_BYTES, cancelled)
    }
    #[cfg(target_os = "windows")]
    {
        windows_board::current(MAX_TEXT_BYTES, cancelled)
    }
    #[cfg(target_os = "linux")]
    {
        linux::read(cancelled)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err(if cancelled() {
            ClipboardError::Cancelled
        } else {
            ClipboardError::UnsupportedBackend
        })
    }
}

/// Write only the reviewed result from the trusted Copy command.
pub fn copy_result(text: &str) -> Result<(), ClipboardError> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(ClipboardError::TooLarge);
    }
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSPasteboard;
        objc2::rc::autoreleasepool(|_| {
            objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
                macos::write(&NSPasteboard::generalPasteboard(), text)
            }))
            .map_err(|_| ClipboardError::Access)?
        })
    }
    #[cfg(target_os = "windows")]
    {
        windows_board::write(text)
    }
    #[cfg(target_os = "linux")]
    {
        linux::write(text)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    Err(ClipboardError::UnsupportedBackend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_unicode_snapshot_limits_encoded_bytes_and_requires_a_terminator() {
        assert_eq!(decode_wide(&[0], 2, 4).unwrap(), b"");
        assert_eq!(
            decode_wide(&[0xd83d, 0xde3a, 0], 6, 4).unwrap(),
            "😺".as_bytes()
        );
        assert_eq!(
            decode_wide(&[0xd83d, 0xde3a, 0], 6, 3),
            Err(ClipboardError::TooLarge)
        );
        assert_eq!(
            decode_wide(&[0xd83d, 0], 4, 4),
            Err(ClipboardError::InvalidEncoding)
        );
        assert_eq!(
            decode_wide(&[65, 66], 4, 4),
            Err(ClipboardError::InvalidEncoding)
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires a foreground Windows desktop and SDM_DISPOSABLE_CLIPBOARD=1"]
    fn foreground_windows_result_roundtrips_and_rejects_contention() {
        use windows::{
            core::w,
            Win32::{
                Foundation::HWND,
                System::{
                    DataExchange::{CloseClipboard, OpenClipboard},
                    Ole::{OleInitialize, OleUninitialize},
                },
                UI::WindowsAndMessaging::{
                    CreateWindowExW, DestroyWindow, GetForegroundWindow, SetForegroundWindow,
                    WS_OVERLAPPEDWINDOW, WS_VISIBLE,
                },
            },
        };
        assert_eq!(
            std::env::var("SDM_DISPOSABLE_CLIPBOARD").as_deref(),
            Ok("1")
        );
        struct Foreground(HWND);
        impl Drop for Foreground {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyWindow(self.0);
                    OleUninitialize();
                }
            }
        }
        let window = unsafe {
            OleInitialize(None).unwrap();
            let window = Foreground(
                CreateWindowExW(
                    Default::default(),
                    w!("STATIC"),
                    w!("Clipboard test"),
                    WS_VISIBLE | WS_OVERLAPPEDWINDOW,
                    0,
                    0,
                    320,
                    120,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap(),
            );
            let _ = SetForegroundWindow(window.0);
            assert_eq!(
                GetForegroundWindow(),
                window.0,
                "Bring the disposable test desktop to the foreground"
            );
            window
        };
        let boundary = "😺".repeat(MAX_TEXT_BYTES / 4);
        for text in ["", "α😺\n", &boundary] {
            copy_result(text).unwrap();
            assert_eq!(capture_text(&|| false).unwrap().as_str(), text);
        }
        assert_eq!(copy_result("a\0b"), Err(ClipboardError::InvalidEncoding));
        assert_eq!(
            copy_result(&format!("{boundary}x")),
            Err(ClipboardError::TooLarge)
        );
        let (held, ready) = std::sync::mpsc::channel();
        let (release, done) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || unsafe {
            OpenClipboard(None).unwrap();
            held.send(()).unwrap();
            let _ = done.recv();
            CloseClipboard().unwrap();
        });
        ready.recv().unwrap();
        let busy = copy_result("busy");
        release.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(busy, Err(ClipboardError::Access));
        assert_eq!(capture_text(&|| false).unwrap().as_str(), boundary);
        drop(window);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires a macOS graphical pasteboard service"]
    fn native_named_pasteboard_preserves_unicode_empty_items_and_bounds() {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        use objc2_foundation::{NSData, NSString};
        objc2::rc::autoreleasepool(|_| {
            // This board is unique to the test. Never read or replace the user's general pasteboard.
            let board = NSPasteboard::pasteboardWithUniqueName();
            let format = unsafe { NSPasteboardTypeString };
            assert!(matches!(
                macos::write(&board, &"x".repeat(MAX_TEXT_BYTES + 1)),
                Err(ClipboardError::TooLarge)
            ));
            for text in ["", "α😺\n"] {
                macos::write(&board, text).unwrap();
                assert_eq!(
                    macos::read(&board, MAX_TEXT_BYTES, &|| false)
                        .unwrap()
                        .as_str(),
                    text
                );
            }
            board.clearContents();
            assert!(board.setData_forType(
                Some(&NSData::with_bytes(b"fixture")),
                &NSString::from_str("org.example.synthetic")
            ));
            assert_eq!(
                macos::read(&board, MAX_TEXT_BYTES, &|| false).unwrap_err(),
                ClipboardError::UnsupportedFormat
            );
            board.clearContents();
            assert!(board.setString_forType(&NSString::from_str("😺"), format));
            assert_eq!(
                macos::read(&board, 3, &|| false).unwrap_err(),
                ClipboardError::TooLarge
            );
            board.clearContents();
        });
    }

    #[test]
    fn clipboard_reads_one_snapshot_and_never_truncates_utf8() {
        let text = "😺".repeat(MAX_TEXT_BYTES / 4);
        let read = |bytes| snapshot(MAX_TEXT_BYTES, &|| false, || 3, |_| Ok(bytes));
        assert_eq!(read(text.as_bytes().to_vec()).unwrap().as_str(), text);
        assert_eq!(
            read(format!("{text}a").into_bytes()).unwrap_err(),
            ClipboardError::TooLarge
        );
        assert!(read(vec![]).unwrap().is_empty());
        assert_eq!(
            read(vec![255]).unwrap_err(),
            ClipboardError::InvalidEncoding
        );
        let revision = std::cell::Cell::new(1);
        assert_eq!(
            snapshot(
                8,
                &|| false,
                || revision.get(),
                |_| {
                    revision.set(2);
                    Ok(vec![])
                }
            )
            .unwrap_err(),
            ClipboardError::Changed
        );
        let cancel = std::cell::Cell::new(false);
        assert_eq!(
            snapshot(
                8,
                &|| cancel.get(),
                || 1,
                |_| {
                    cancel.set(true);
                    Ok(vec![])
                }
            )
            .unwrap_err(),
            ClipboardError::Cancelled
        );
        for error in [
            ClipboardError::NoText,
            ClipboardError::UnsupportedFormat,
            ClipboardError::Access,
        ] {
            let expected = format!("{error:?}");
            assert_eq!(
                format!(
                    "{:?}",
                    snapshot(8, &|| false, || 1, |_| Err(error)).unwrap_err()
                ),
                expected
            );
        }
    }
}
