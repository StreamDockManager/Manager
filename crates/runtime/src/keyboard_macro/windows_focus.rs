//! Out-of-context WinEvent observers keep a sticky foreground/window lifetime
//! generation. Dedicated message pumping never installs a low-level keyboard hook
use super::super::focus::{Observer, State};
use super::Target;
use ::windows::Win32::{
    Foundation::HWND,
    UI::{
        Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
        WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, TranslateMessage, EVENT_OBJECT_DESTROY,
            EVENT_SYSTEM_FOREGROUND, MSG, PM_REMOVE, WINEVENT_OUTOFCONTEXT, WM_QUIT,
        },
    },
};
use std::{
    cell::Cell,
    ptr,
    sync::{mpsc, Arc},
    time::Duration,
};
struct Context {
    signal: Arc<State>,
    target: Cell<isize>,
}
thread_local! { static CONTEXT: Cell<*const Context> = const { Cell::new(ptr::null()) }; }
// Observe native focus changes and invalidate the captured target as needed
unsafe extern "system" fn changed(
    _: HWINEVENTHOOK,
    event: u32,
    window: HWND,
    object: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    CONTEXT.with(|slot| {
        if let Some(context) = slot.get().as_ref() {
            if event == EVENT_SYSTEM_FOREGROUND
                || (event == EVENT_OBJECT_DESTROY
                    && object == 0
                    && window.0 as isize == context.target.get())
            {
                context.signal.changed();
            }
        }
    });
}
struct Hooks(Vec<HWINEVENTHOOK>);
impl Hooks {
    // Add an entry to Windows focus
    fn add(&mut self, event: u32, pid: u32) -> Result<(), String> {
        let hook = unsafe {
            SetWinEventHook(
                event,
                event,
                None,
                Some(changed),
                pid,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        if hook.0.is_null() {
            return Err("Keyboard macro foreground observer could not be installed".into());
        }
        self.0.push(hook);
        Ok(())
    }
}
impl Drop for Hooks {
    // Release this Windows focus owner when it leaves scope
    fn drop(&mut self) {
        for hook in self.0.drain(..).rev() {
            if !unsafe { UnhookWinEvent(hook) }.as_bool() {
                CONTEXT.with(|slot| unsafe {
                    if let Some(context) = slot.get().as_ref() {
                        context.signal.fail();
                    }
                });
            }
        }
        CONTEXT.with(|slot| slot.set(ptr::null()));
    }
}
// Drain messages
fn drain_messages() -> Result<(), String> {
    let mut message = MSG::default();
    let mut count = 0;
    while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        if message.message == WM_QUIT {
            return Err("Keyboard macro foreground observer exited".into());
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        count += 1;
        if count > 256 {
            return Err("Keyboard macro foreground observer queue overflowed".into());
        }
    }
    Ok(())
}
// Start the Windows focus operation
pub(super) fn start() -> Result<Observer, String> {
    Observer::start(run)
}
// Drive the Windows focus operation
fn run(signal: Arc<State>, startup: mpsc::SyncSender<Result<(), String>>) -> Result<(), String> {
    let context = Context {
        signal: signal.clone(),
        target: Cell::new(0),
    };
    CONTEXT.with(|slot| slot.set(&context));
    let mut hooks = Hooks(vec![]);
    hooks.add(EVENT_SYSTEM_FOREGROUND, 0)?; // EVENT_SYSTEM_FOREGROUND, including Manager itself
    let target = Target::current()?;
    context.target.set(target.window);
    hooks.add(EVENT_OBJECT_DESTROY, target.pid)?; // EVENT_OBJECT_DESTROY on the original process
                                                  // Close the capture/install race; callbacks already latch any intermediate A/B/A
    drain_messages()?;
    if target != Target::current()? {
        signal.changed();
    }
    signal.ready();
    startup
        .send(Ok(()))
        .map_err(|_| "Keyboard macro foreground observer was canceled")?;
    while !signal.stopped() {
        drain_messages()?;
        let request = signal.requested_check();
        if target != Target::current()? {
            signal.changed();
        }
        signal.complete_check(request);
        signal.pulse();
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
