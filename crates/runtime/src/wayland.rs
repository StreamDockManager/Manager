//! Authorized Remote Desktop portal input. Never falls back to XWayland injection
use ashpd::desktop::{
    remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions},
    PersistMode, Session,
};
use futures_util::StreamExt;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc, OnceLock,
};
enum Request {
    Authorize(mpsc::Sender<Result<(), String>>),
    Keys(Vec<String>, mpsc::Sender<Result<(), String>>),
    State(String, bool, mpsc::Sender<Result<(), String>>),
    Physical(PhysicalRequest),
    Pointer(Pointer, mpsc::Sender<Result<(), String>>),
    Shutdown,
}
static WORKER: OnceLock<tokio::sync::mpsc::UnboundedSender<Request>> = OnceLock::new();
// Recorded releases must bypass a backlog of ordinary portal requests
static PHYSICAL_CLEANUP: OnceLock<tokio::sync::mpsc::UnboundedSender<PhysicalRequest>> =
    OnceLock::new();
type PhysicalGuard = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;
struct PhysicalRequest {
    code: u16,
    pressed: bool,
    generation: usize,
    deadline: std::time::Instant,
    canceled: Arc<AtomicBool>,
    guard: PhysicalGuard,
    reply: mpsc::Sender<Result<(), String>>,
}

enum Pointer {
    Move(i32, i32, bool),
    Button(String, bool),
    Scroll(i32, i32),
}
static SESSION_GENERATION: AtomicUsize = AtomicUsize::new(0);
static POINTER_AUTHORIZED: AtomicBool = AtomicBool::new(false);
static AUTHORIZED: AtomicBool = AtomicBool::new(false);
struct Connection {
    proxy: RemoteDesktop,
    session: Arc<Session<RemoteDesktop>>,
    revoked: Arc<AtomicBool>,
}
// Run the worker’s queued operations
fn worker() -> &'static tokio::sync::mpsc::UnboundedSender<Request> {
    WORKER.get_or_init(||{
    let (tx,mut rx)=tokio::sync::mpsc::unbounded_channel();
    let (cleanup_tx,mut cleanup_rx)=tokio::sync::mpsc::unbounded_channel();
    let _=PHYSICAL_CLEANUP.set(cleanup_tx);
    std::thread::spawn(move||{let Ok(rt)=tokio::runtime::Builder::new_current_thread().enable_all().build()else{return};rt.block_on(async move{
        let mut connection:Option<Connection>=None;
        loop {let request=tokio::select!{biased;Some(request)=cleanup_rx.recv()=>Request::Physical(request),request=rx.recv()=>match request{Some(request)=>request,None=>break}};match request{
            Request::Authorize(reply)=>{
                SESSION_GENERATION.fetch_add(1,Ordering::SeqCst);crate::builtins::invalidate_input();POINTER_AUTHORIZED.store(false,Ordering::SeqCst);
                if let Some(old)=connection.take(){let _=old.session.close().await;}
                AUTHORIZED.store(false,Ordering::SeqCst);
                let result=tokio::time::timeout(std::time::Duration::from_secs(120),connect()).await;
                let result=match result{Ok(Ok(next))=>{connection=Some(next);AUTHORIZED.store(true,Ordering::SeqCst);Ok(())},Ok(Err(e))=>Err(e),Err(_)=>Err("Portal authorization expired; request access again.".into())};let _=reply.send(result);
            },
            Request::Keys(keys,reply)=>{let result=match connection.as_ref(){Some(c)if !c.revoked.load(Ordering::SeqCst)=>send_keys(c,&keys).await,_=>Err("Keyboard access is not authorized. Request Wayland access in Settings.".into())};if result.is_err(){AUTHORIZED.store(false,Ordering::SeqCst);if let Some(old)=connection.take(){let _=old.session.close().await;}}let _=reply.send(result);},
            Request::State(key,pressed,reply)=>{let result=match connection.as_ref(){Some(c)if !c.revoked.load(Ordering::SeqCst)=>match keysym(&key){Ok(symbol)=>c.proxy.notify_keyboard_keysym(&c.session,symbol,if pressed{KeyState::Pressed}else{KeyState::Released},Default::default()).await.map_err(|e|e.to_string()),Err(e)=>Err(e)},_=>Err("Keyboard access is not authorized. Request Wayland access in Settings.".into())};let _=reply.send(result);},
            Request::Physical(request)=>{let result=match connection.as_ref(){Some(c)if !c.revoked.load(Ordering::SeqCst)=>send_physical(c,&request).await,_=>Err("Wayland physical keyboard access is unavailable".into())};let _=request.reply.send(result);},
            Request::Pointer(event,reply)=>{let result=match connection.as_ref(){Some(c)if !c.revoked.load(Ordering::SeqCst)&&pointer_authorized()=>match event{
                Pointer::Move(x,y,true)=>c.proxy.notify_pointer_motion(&c.session,x as f64,y as f64,Default::default()).await.map_err(|e|e.to_string()),
                Pointer::Move(_,_,false)=>Err("Absolute pointer movement requires an authorized screen mapping; use relative coordinates".into()),
                Pointer::Button(name,pressed)=>{let code=match name.as_str(){"left"=>272,"right"=>273,"middle"=>274,"back"=>275,"forward"=>276,_=>0};if code==0{Err("Unsupported pointer button".into())}else{c.proxy.notify_pointer_button(&c.session,code,if pressed{KeyState::Pressed}else{KeyState::Released},Default::default()).await.map_err(|e|e.to_string())}},
                Pointer::Scroll(x,y)=>c.proxy.notify_pointer_axis(&c.session,x as f64*15.0,y as f64*15.0,Default::default()).await.map_err(|e|e.to_string())
            },_=>Err("Pointer access is not authorized. Request Wayland access in Settings.".into())};let _=reply.send(result);},
            Request::Shutdown=>break,
        }}if let Some(c)=connection{let _=c.session.close().await;}AUTHORIZED.store(false,Ordering::SeqCst);
    });});tx
})
}
// Report the authorized condition for wayland
pub fn authorized() -> bool {
    AUTHORIZED.load(Ordering::SeqCst)
}
// Check authorization for the wayland request
pub fn authorize() -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    worker()
        .send(Request::Authorize(tx))
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}
// Send a logical hotkey request to the Wayland worker and wait for its result
pub fn hotkey(keys: &[String]) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    worker()
        .send(Request::Keys(keys.to_vec(), tx))
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}
// Send authorized logical key-state changes while treating releases after authorization loss as harmless
pub fn key_state(key: &str, pressed: bool) -> Result<(), String> {
    if !authorized() {
        return if pressed {
            Err("Keyboard access is not authorized. Request Wayland access in Settings.".into())
        } else {
            Ok(())
        };
    }
    let (tx, rx) = mpsc::channel();
    worker()
        .send(Request::State(key.into(), pressed, tx))
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}
// Read the current Wayland keyboard authorization generation
pub(crate) fn keyboard_generation() -> usize {
    SESSION_GENERATION.load(Ordering::SeqCst)
}
// Reject expired, canceled, stale or unsupported physical-key requests before invoking their guard
fn physical_request_valid(
    request: &PhysicalRequest,
    generation: usize,
    authorized: bool,
    now: std::time::Instant,
) -> Result<(), String> {
    if request.canceled.load(Ordering::SeqCst) || now >= request.deadline {
        return Err("Wayland physical keyboard request expired".into());
    }
    if !authorized || request.generation != generation {
        return Err("Wayland physical keyboard authorization changed".into());
    }
    if crate::keyboard_macro::platform::linux_mapping::physical_from_evdev(request.code).is_none() {
        return Err("Unsupported Wayland physical key".into());
    }
    (request.guard)()?;
    Ok(())
}
// Send physical
async fn send_physical(connection: &Connection, request: &PhysicalRequest) -> Result<(), String> {
    physical_request_valid(
        request,
        keyboard_generation(),
        authorized(),
        std::time::Instant::now(),
    )?;
    // The portal's physical contract is Linux evdev, not X11 keycodes/keysyms
    // A timed-out press has uncertain delivery; the caller retains ownership and
    // submits a matching release through the independent priority lane
    tokio::time::timeout(
        std::time::Duration::from_millis(100),
        connection.proxy.notify_keyboard_keycode(
            &connection.session,
            i32::from(request.code),
            if request.pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            Default::default(),
        ),
    )
    .await
    .map_err(|_| "Wayland physical keyboard output timed out")?
    .map_err(|_| "Wayland physical keyboard output failed".into())
}
// Submit a generation-checked physical key transition with a bounded deadline and dedicated release cleanup
pub(crate) fn physical_key_state(
    code: u16,
    pressed: bool,
    generation: usize,
    guard: PhysicalGuard,
) -> Result<(), String> {
    let (reply, result) = mpsc::channel();
    let canceled = Arc::new(AtomicBool::new(false));
    let request = PhysicalRequest {
        code,
        pressed,
        generation,
        deadline: std::time::Instant::now() + std::time::Duration::from_millis(500),
        canceled: canceled.clone(),
        guard,
        reply,
    };
    physical_request_valid(
        &request,
        keyboard_generation(),
        authorized(),
        std::time::Instant::now(),
    )?;
    let normal = worker();
    if pressed {
        normal
            .send(Request::Physical(request))
            .map_err(|_| "Wayland physical keyboard worker stopped")?;
    } else {
        PHYSICAL_CLEANUP
            .get()
            .ok_or("Wayland physical keyboard cleanup is unavailable")?
            .send(request)
            .map_err(|_| "Wayland physical keyboard cleanup stopped")?;
    }
    match result.recv_timeout(std::time::Duration::from_millis(600)) {
        Ok(result) => result,
        Err(_) => {
            canceled.store(true, Ordering::SeqCst);
            Err("Wayland physical keyboard request did not complete".into())
        }
    }
}

// Report the pointer authorized condition for wayland
pub fn pointer_authorized() -> bool {
    authorized() && POINTER_AUTHORIZED.load(Ordering::SeqCst)
}
// Route authorized pointer requests to the Wayland worker and permit harmless late button releases
fn pointer(event: Pointer) -> Result<(), String> {
    if !pointer_authorized() {
        return if matches!(event, Pointer::Button(_, false)) {
            Ok(())
        } else {
            Err("Pointer access is not authorized. Request Wayland access in Settings.".into())
        };
    }
    let (tx, rx) = mpsc::channel();
    worker()
        .send(Request::Pointer(event, tx))
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}
// Request an absolute or relative pointer movement through the authorized Wayland path
pub fn pointer_move(x: i32, y: i32, relative: bool) -> Result<(), String> {
    pointer(Pointer::Move(x, y, relative))
}
// Request a named pointer button transition through the authorized Wayland path
pub fn pointer_button(name: &str, pressed: bool) -> Result<(), String> {
    pointer(Pointer::Button(name.into(), pressed))
}
// Request pointer scrolling through the authorized Wayland path
pub fn pointer_scroll(x: i32, y: i32) -> Result<(), String> {
    pointer(Pointer::Scroll(x, y))
}
// Shut down the wayland worker
pub fn shutdown() {
    if let Some(worker) = WORKER.get() {
        let _ = worker.send(Request::Shutdown);
    }
}
// Establish the wayland connection
async fn connect() -> Result<Connection, String> {
    let proxy = RemoteDesktop::new().await.map_err(|e| e.to_string())?;
    let session = Arc::new(
        proxy
            .create_session(Default::default())
            .await
            .map_err(|e| e.to_string())?,
    );
    use crate::credentials::CredentialProvider;
    let credentials = crate::credentials::OsCredentials;
    let token = credentials.read("wayland-restore-token").ok();
    let options = SelectDevicesOptions::default()
        .set_devices(Some(DeviceType::KEYBOARD | DeviceType::POINTER))
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(token.as_deref());
    proxy
        .select_devices(&session, options)
        .await
        .map_err(|e| e.to_string())?
        .response()
        .map_err(|e| e.to_string())?;
    let response = proxy
        .start(&session, None, Default::default())
        .await
        .map_err(|e| e.to_string())?
        .response()
        .map_err(|e| e.to_string())?;
    if !response.devices().contains(DeviceType::KEYBOARD) {
        let _ = session.close().await;
        return Err(
            "Keyboard permission was denied; other device functions remain available.".into(),
        );
    }
    POINTER_AUTHORIZED.store(
        response.devices().contains(DeviceType::POINTER),
        Ordering::SeqCst,
    );
    if let Some(token) = response.restore_token() {
        let _ = credentials.write("wayland-restore-token", token);
    }
    let generation = SESSION_GENERATION.load(Ordering::SeqCst);
    let revoked = Arc::new(AtomicBool::new(false));
    let watch = session.clone();
    let flag = revoked.clone();
    tokio::spawn(async move {
        if let Ok(stream) = watch.receive_closed().await {
            futures_util::pin_mut!(stream);
            let _ = stream.next().await;
        }
        flag.store(true, Ordering::SeqCst);
        if SESSION_GENERATION.load(Ordering::SeqCst) == generation {
            AUTHORIZED.store(false, Ordering::SeqCst);
            POINTER_AUTHORIZED.store(false, Ordering::SeqCst);
            crate::builtins::invalidate_input();
        }
    });
    Ok(Connection {
        proxy,
        session,
        revoked,
    })
}
// Translate a supported logical key name into its XKB keysym
fn keysym(key: &str) -> Result<i32, String> {
    Ok(match key.to_lowercase().as_str() {
        "ctrl" | "control" => 0xffe3,
        "shift" => 0xffe1,
        "alt" => 0xffe9,
        "meta" | "super" | "cmd" => 0xffeb,
        "enter" => 0xff0d,
        "tab" => 0xff09,
        "escape" => 0xff1b,
        "backspace" => 0xff08,
        "delete" => 0xffff,
        "insert" => 0xff63,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "plus" => 0x2b,
        "space" => 0x20,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        name if name.starts_with('f')
            && name[1..]
                .parse::<i32>()
                .is_ok_and(|n| (1..=24).contains(&n)) =>
        {
            0xffbd + name[1..].parse::<i32>().unwrap()
        }
        _ => {
            let mut chars = key.chars();
            let code = chars.next().ok_or("empty key")? as i32;
            if chars.next().is_some() {
                return Err("unsupported keysym".into());
            }
            if code > 255 {
                code | 0x01000000
            } else {
                code
            }
        }
    })
}
// Send keys
async fn send_keys(connection: &Connection, keys: &[String]) -> Result<(), String> {
    let mut held = vec![];
    let result = async {
        for key in keys {
            let symbol = keysym(key)?;
            connection
                .proxy
                .notify_keyboard_keysym(
                    &connection.session,
                    symbol,
                    KeyState::Pressed,
                    Default::default(),
                )
                .await
                .map_err(|e| e.to_string())?;
            held.push(symbol);
        }
        Ok(())
    }
    .await;
    for symbol in held.into_iter().rev() {
        let _ = connection
            .proxy
            .notify_keyboard_keysym(
                &connection.session,
                symbol,
                KeyState::Released,
                Default::default(),
            )
            .await;
    }
    result
}

#[cfg(test)]
mod physical_keyboard_tests {
    use super::*;
    #[test]
    // Verify that stale expired and invalid physical requests cannot emit late input
    fn stale_expired_and_invalid_physical_requests_cannot_emit_late_input() {
        let now = std::time::Instant::now();
        let (reply, _) = mpsc::channel();
        let mut request = PhysicalRequest {
            code: 30,
            pressed: true,
            generation: 7,
            deadline: now + std::time::Duration::from_millis(100),
            canceled: Arc::new(AtomicBool::new(false)),
            guard: Arc::new(|| Ok(())),
            reply,
        };
        assert!(physical_request_valid(&request, 7, true, now).is_ok());
        assert!(physical_request_valid(&request, 8, true, now).is_err());
        assert!(physical_request_valid(&request, 7, false, now).is_err());
        assert!(physical_request_valid(&request, 7, true, request.deadline).is_err());
        request.canceled.store(true, Ordering::SeqCst);
        assert!(physical_request_valid(&request, 7, true, now).is_err());
        request.canceled.store(false, Ordering::SeqCst);
        let external_stop = Arc::new(AtomicBool::new(false));
        let queued_stop = external_stop.clone();
        request.guard = Arc::new(move || {
            if queued_stop.load(Ordering::Acquire) {
                Err("run canceled while queued".into())
            } else {
                Ok(())
            }
        });
        assert!(physical_request_valid(&request, 7, true, now).is_ok());
        external_stop.store(true, Ordering::Release);
        assert!(physical_request_valid(&request, 7, true, now).is_err());
        request.guard = Arc::new(|| Err("observer revoked while queued".into()));
        assert!(physical_request_valid(&request, 7, true, now).is_err());
        request.pressed = false;
        request.guard = Arc::new(|| Ok(())); // independent release ownership guard
        assert!(physical_request_valid(&request, 7, true, now).is_ok());
        request.code = u16::MAX;
        assert!(physical_request_valid(&request, 7, true, now).is_err());
    }
}
