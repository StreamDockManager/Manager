//! Resolve layout-dependent macOS keys without calling HIToolbox on an input worker
use enigo::Key;

// Resolve the supplied keyboard layout request
pub(crate) fn resolve(keys: Vec<Key>) -> Result<Vec<Key>, String> {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use std::{sync::mpsc, time::Duration};

        if !keys
            .iter()
            .any(|key| matches!(key, Key::Unicode(c) if *c != '\0'))
        {
            return Ok(keys);
        }
        if MainThreadMarker::new().is_some() {
            return resolve_on_main(keys);
        }
        let (reply, result) = mpsc::sync_channel(1);
        dispatch2::DispatchQueue::main().exec_async(move || {
            // Only read the keyboard layout here. A timed-out request must never
            // inject delayed input when the application starts servicing events
            let _ = reply.send(resolve_on_main(keys));
        });
        result
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "Keyboard layout lookup timed out; no shortcut was pressed. Retry while Manager is responsive.".to_string())?
    }
    #[cfg(not(target_os = "macos"))]
    Ok(keys)
}

#[cfg(target_os = "macos")]
// Resolve on main
fn resolve_on_main(keys: Vec<Key>) -> Result<Vec<Key>, String> {
    // Enigo's Unicode conversion calls TISGetInputSourceProperty, which macOS
    // requires on the main dispatch queue. Keep raw codes for both down and up;
    // neither event delivery nor Enigo's Drop may repeat the layout lookup
    let _main = objc2::MainThreadMarker::new()
        .ok_or("Keyboard layout lookup requires the main application thread")?;
    objc2::rc::autoreleasepool(|_| {
        keys.into_iter()
            .map(|key| match key {
                Key::Unicode(c) if c != '\0' => u16::try_from(key)
                    .map(|code| Key::Other(u32::from(code)))
                    .map_err(|_| {
                        "Could not resolve shortcut key for the current keyboard layout".into()
                    }),
                other => Ok(other),
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify that named keys need no main run loop
    fn named_keys_need_no_main_run_loop() {
        let keys = vec![
            Key::Control,
            Key::Shift,
            Key::F9,
            Key::F10,
            Key::Unicode('\0'),
        ];
        assert_eq!(resolve(keys.clone()).unwrap(), keys);
    }

    #[cfg(target_os = "macos")]
    #[test]
    // Verify that layout conversion rejects background threads before hitoolbox
    fn layout_conversion_rejects_background_threads_before_hitoolbox() {
        assert!(resolve_on_main(vec![Key::Unicode('m')])
            .unwrap_err()
            .contains("main application thread"));
    }
}
