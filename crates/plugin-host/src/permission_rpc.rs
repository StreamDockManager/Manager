//! Manager-owned, action-scoped services. A launch session authenticates the package;
//! a separate single-use receipt authenticates a physical press at a visible binding.
use super::*;
use crate::permissions::{PermissionDeclaration, PermissionScope};
use crate::vault_references::{
    RecipeEntry, RecipeValue, ReferenceScope, References, ReviewVersion,
};
use std::time::Instant;
use stream_dock_manager_storage::{
    permissions::{Decisions, PermissionOwner},
    Storage,
};
use zeroize::Zeroizing;

pub const PROTOCOL: &str = "plugin-permissions-v1";
pub const BROKER: &str = "plugin-http-broker-v1";
pub const NOTIFICATIONS: &str = "plugin-notifications-v1";
const NOTIFICATION_MESSAGE_LIMIT: usize = 512;
const NOTIFICATION_WINDOW: Duration = Duration::from_secs(30);
const NOTIFICATION_GLOBAL_LIMIT: usize = 8;
const INPUT_TTL: Duration = Duration::from_secs(10);
const WINDOW_TTL: Duration = Duration::from_secs(90);
pub const TEXT_LIMIT: usize = 64 * 1024;
pub const SUMMARY_ORIGIN: &str = "https://api.example.test";

#[derive(Clone)]
pub enum WebhookEndpoint {
    Https { certificate: String },
    Http { origin: String },
}
impl WebhookEndpoint {
    pub fn origin(&self) -> &str {
        match self {
            Self::Https { .. } => permissions::WEBHOOK_ORIGIN,
            Self::Http { origin } => origin,
        }
    }
    fn scope(&self) -> PermissionScope {
        let origin = self.origin().into();
        let recipe = "manager-webhook".into();
        let data_class = "example.ping".into();
        match self {
            Self::Https { .. } => PermissionScope::Https {
                origin,
                recipe,
                data_class,
            },
            Self::Http { .. } => PermissionScope::Http {
                origin,
                recipe,
                data_class,
            },
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginNotificationLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// Manager derives the sender from the authenticated installed package.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PluginNotification {
    pub plugin: String,
    pub level: PluginNotificationLevel,
    pub message: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateNotification {
    action: String,
    level: PluginNotificationLevel,
    message: String,
}

fn notification_scope() -> PermissionScope {
    PermissionScope::CreateNotification {
        mode: "plugin-message".into(),
    }
}

fn notification_text(text: &str) -> Result<String, &'static str> {
    if text.len() > NOTIFICATION_MESSAGE_LIMIT {
        return Err("too_large");
    }
    // Whitespace is folded onto one line. Reject non-whitespace controls and
    // directional formatting so text cannot spoof or obscure attribution.
    if text.chars().any(|c| {
        (c.is_control() && !c.is_whitespace())
            || matches!(c, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{206f}')
    }) {
        return Err("invalid_request");
    }
    let message = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if message.is_empty() {
        return Err("invalid_request");
    }
    Ok(message)
}

pub trait Services: Send + Sync {
    fn references(&self) -> Option<&dyn References> {
        None
    }
    fn clipboard_available(&self) -> bool;
    fn notification_available(&self) -> bool {
        false
    }
    fn notify_completed(&self, _plugin: &str) -> Result<(), &'static str> {
        Err("operation_unavailable")
    }
    fn plugin_notifications_available(&self) -> bool {
        false
    }
    fn create_plugin_notification(
        &self,
        _notification: &PluginNotification,
    ) -> Result<(), &'static str> {
        Err("operation_unavailable")
    }
    fn copy_available(&self) -> bool;
    fn capture(&self, cancelled: &dyn Fn() -> bool) -> Result<Zeroizing<String>, &'static str>;
    fn copy(&self, text: &str) -> Result<(), &'static str>;
    fn summarize(
        &self,
        reviewed: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Zeroizing<String>, &'static str>;
    fn credentialed_summary(
        &self,
        _text: &str,
        _authorization: &str,
        _cancelled: &dyn Fn() -> bool,
    ) -> Result<Zeroizing<String>, &'static str> {
        Err("operation_unavailable")
    }
    fn webhook_available(&self) -> bool {
        false
    }
    fn webhook(
        &self,
        _endpoint: &WebhookEndpoint,
        _cancelled: &dyn Fn() -> bool,
    ) -> Result<(), &'static str> {
        Err("operation_unavailable")
    }
    fn open(&self, window: &str) -> Result<(), &'static str>;
    fn close(&self, window: &str);
    fn changed(&self, _plugin: &str) {}
}

#[derive(Clone)]
pub(super) struct Session {
    owner: PermissionOwner,
    declaration: PermissionDeclaration,
}
impl Session {
    pub(super) fn declares_plugin_notifications(&self) -> bool {
        let scope = notification_scope();
        self.declaration
            .actions
            .values()
            .any(|group| group.required.contains(&scope) || group.optional.contains(&scope))
    }
    pub(super) fn has_reference_declaration(&self) -> bool {
        self.declaration.actions.values().any(|g| {
            g.required
                .iter()
                .chain(&g.optional)
                .any(|s| matches!(s, PermissionScope::Access { .. }))
        })
    }
}
pub(super) fn scope_key(scope: &PermissionScope) -> String {
    serde_json::to_string(scope).expect("serializable permission scope")
}
fn available(scope: &PermissionScope, services: Option<&dyn Services>) -> bool {
    let Some(services) = services else {
        return false;
    };
    match scope {
        PermissionScope::Access {
            recipe, operation, ..
        } => {
            services.references().is_some()
                && matches!(
                    (recipe.as_str(), operation.as_str()),
                    ("clipboard-summary", "authorize-request")
                )
        }
        PermissionScope::Popup { .. } => true,
        PermissionScope::Notification { .. } => services.notification_available(),
        PermissionScope::CreateNotification { .. } => services.plugin_notifications_available(),
        PermissionScope::ClipboardWrite { .. } => services.copy_available(),
        PermissionScope::ClipboardRead { .. } => services.clipboard_available(),
        PermissionScope::Http { .. } => services.webhook_available(),
        PermissionScope::Https { recipe, .. } => {
            (matches!(recipe.as_str(), "manager-webhook" | "plugin-webhook")
                && services.webhook_available())
                || (recipe == "clipboard-summary" && services.clipboard_available())
        }
        _ => false,
    }
}
fn granted(decisions: &Decisions, action: &str, scope: &PermissionScope) -> bool {
    decisions.scopes.get(&(action.to_owned(), scope_key(scope))) == Some(&true)
}

#[derive(Clone)]
struct Activation {
    session: Session,
    alive: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    valid: Arc<AtomicBool>,
    context: String,
    action: String,
    revision: u64,
    expires: Instant,
}

impl Activation {
    fn live(&self) -> bool {
        self.alive.load(Ordering::Acquire)
            && !self.stopped.load(Ordering::Acquire)
            && self.valid.load(Ordering::Acquire)
    }
}

enum WindowContent {
    Permissions {
        vault_version: Option<ReviewVersion>,
        scopes: Vec<PermissionScope>,
        request: String,
    },
    Select {
        request: String,
        scope: Box<ReferenceScope>,
        version: ReviewVersion,
        start: usize,
    },
    Transfer(Zeroizing<String>),
    Webhook(WebhookEndpoint),
    Failed,
    Sending(String, String),
    Result(Zeroizing<String>),
}

struct Window {
    activation: Activation,
    content: WindowContent,
    entry: Option<RecipeEntry>,
    expires: Instant,
}

#[derive(Default)]
pub struct Workflow {
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    services: Option<Arc<dyn Services>>,
    tokens: HashMap<String, Activation>,
    windows: HashMap<String, Window>,
    wake: Vec<(std::sync::Weak<AtomicBool>, std::sync::Weak<mio::Waker>)>,
    notifications: VecDeque<(Instant, String)>,
    notices: VecDeque<(Arc<AtomicBool>, Value)>,
}
impl State {
    fn notice(&mut self, lease: &Arc<AtomicBool>, event: Value) {
        self.notices.retain(|(l, _)| l.load(Ordering::Acquire));
        if self.notices.len() < 1024 && lease.load(Ordering::Acquire) {
            self.notices.push_back((lease.clone(), event));
            self.wake.retain(|(l, w)| {
                let Some(l) = l.upgrade().filter(|l| l.load(Ordering::Acquire)) else {
                    return false;
                };
                let Some(w) = w.upgrade() else { return false };
                if Arc::ptr_eq(&l, lease) {
                    let _ = w.wake();
                }
                true
            });
        }
    }
    fn finish(&mut self, id: &str, outcome: &str, revision: Option<u64>) {
        if let Some(w) = self.windows.remove(id) {
            w.activation.valid.store(false, Ordering::Release);
            if let WindowContent::Permissions { ref request, .. } = w.content {
                self.notice(&w.activation.alive,json!({"event":"streamDockManagerPermissionRequestResult","request_id":request,"review_id":id,"outcome":outcome,"revision":revision.unwrap_or(w.activation.revision).to_string()}));
            }
            if let WindowContent::Select { ref request, .. } = w.content {
                self.notice(&w.activation.alive,json!({"event":"streamDockManagerVaultSelectionResult","request_id":request,"review_id":id,"outcome":outcome}));
            }
            if let Some(s) = &self.services {
                s.changed(&w.activation.session.owner.plugin);
                s.close(id);
            }
        }
    }
    fn prune(&mut self) {
        let now = Instant::now();
        // Keep a short bounded grace period to distinguish an expired press from a
        // missing token on its originating connection. Neither can be redeemed.
        self.tokens
            .retain(|_, a| a.live() && now < a.expires + INPUT_TTL);
        let stale: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(id, w)| {
                if !w.activation.live() {
                    Some((id.clone(), "cancelled"))
                } else if now >= w.expires {
                    Some((id.clone(), "timeout"))
                } else {
                    None
                }
            })
            .collect();
        for (id, outcome) in stale {
            self.finish(&id, outcome, None);
        }
    }
    fn admit_notification(&mut self, owner: &str) -> Result<Instant, &'static str> {
        let now = Instant::now();
        self.notifications
            .retain(|(t, _)| now.duration_since(*t) < NOTIFICATION_WINDOW);
        if self.notifications.len() >= NOTIFICATION_GLOBAL_LIMIT
            || self.notifications.iter().any(|(_, plugin)| plugin == owner)
        {
            return Err("rate_limited");
        }
        Ok(now)
    }
    fn invalidate(&mut self, predicate: impl Fn(&Activation) -> bool) {
        for a in self
            .tokens
            .values()
            .chain(self.windows.values().map(|w| &w.activation))
            .filter(|a| predicate(a))
        {
            a.valid.store(false, Ordering::Release);
        }
        self.prune();
    }
}
impl Workflow {
    pub fn attach(&self, services: Arc<dyn Services>) {
        self.state.lock().unwrap().services = Some(services);
    }
    pub(super) fn invalidate_context(&self, context: &str) {
        self.state
            .lock()
            .unwrap()
            .invalidate(|a| a.context == context);
    }
    pub(super) fn invalidate_owner(&self, owner: &str) {
        self.state
            .lock()
            .unwrap()
            .invalidate(|a| a.session.owner.plugin == owner);
    }
    pub(super) fn register_socket(&self, lease: &Arc<AtomicBool>, wake: &Arc<mio::Waker>) {
        let mut s = self.state.lock().unwrap();
        s.wake
            .retain(|(l, _)| l.upgrade().is_some_and(|l| l.load(Ordering::Acquire)));
        s.wake.push((Arc::downgrade(lease), Arc::downgrade(wake)));
    }
    pub(super) fn close_vault_windows(&self) {
        let mut s = self.state.lock().unwrap();
        let ids: Vec<_> = s
            .windows
            .iter()
            .filter(|(_, w)| {
                matches!(
                    w.content,
                    WindowContent::Select { .. }
                        | WindowContent::Permissions {
                            vault_version: Some(_),
                            ..
                        }
                ) || w.entry.is_some()
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            s.finish(&id, "cancelled", None);
        }
    }
    pub fn expire(&self) {
        self.state.lock().unwrap().prune();
    }
    pub(super) fn notices(&self, lease: &Arc<AtomicBool>) -> Vec<Value> {
        let mut state = self.state.lock().unwrap();
        let mut result = Vec::new();
        state.notices.retain(|(l, v)| {
            if Arc::ptr_eq(lease, l) {
                result.push(v.clone());
                false
            } else {
                l.load(Ordering::Acquire)
            }
        });
        result
    }
    pub fn cancel(&self, id: &str) {
        self.state.lock().unwrap().finish(id, "cancelled", None);
    }
    fn start(
        &self,
        session: Session,
        alive: Arc<AtomicBool>,
        stopped: Arc<AtomicBool>,
        context: String,
        action: &str,
        revision: u64,
    ) -> Option<String> {
        let mut s = self.state.lock().unwrap();
        s.prune();
        if !alive.load(Ordering::Acquire)
            || s.services.is_none()
            || s.tokens.len() >= 128
            || s.tokens
                .values()
                .filter(|a| a.session.owner.plugin == session.owner.plugin)
                .count()
                >= 16
            || !session.declaration.actions.contains_key(action)
        {
            return None;
        }
        let id = uuid::Uuid::new_v4().to_string();
        s.tokens.insert(
            id.clone(),
            Activation {
                session,
                alive,
                stopped,
                valid: Arc::new(AtomicBool::new(true)),
                context,
                action: action.into(),
                revision,
                expires: Instant::now() + INPUT_TTL,
            },
        );
        Some(id)
    }
    pub(super) fn deliverable(
        &self,
        token: &str,
        alive: Option<&Arc<AtomicBool>>,
        context: &str,
        action: &str,
    ) -> bool {
        self.state
            .lock()
            .unwrap()
            .tokens
            .get(token)
            .is_some_and(|a| {
                a.live()
                    && Instant::now() < a.expires
                    && a.context == context
                    && a.action == action
                    && alive.is_some_and(|v| Arc::ptr_eq(v, &a.alive))
            })
    }
    fn decisions(store: &Storage, a: &Activation) -> Result<Decisions, &'static str> {
        if !a.live() {
            return Err("cancelled");
        }
        let d = store
            .permission_decisions(a.session.owner.clone())
            .map_err(|_| "operation_unavailable")?;
        if d.revision != a.revision {
            return Err("permission_denied");
        }
        Ok(d)
    }
    pub fn view(&self, store: &Storage, id: &str) -> Result<Value, String> {
        let mut s = self.state.lock().unwrap();
        s.prune();
        let w = s.windows.get(id).ok_or("cancelled")?;
        let a = &w.activation;
        let d = Self::decisions(store, a)?;
        let mut v = json!({"plugin":a.session.owner.plugin,"action":a.action,"revision":d.revision.to_string()});
        match &w.content {
            WindowContent::Permissions { scopes, .. } => {
                v["kind"] = json!("permissions");
                v["scopes"] = json!(scopes);
                v["purpose"] = json!(a.session.declaration.actions[&a.action].purpose);
            }
            WindowContent::Select {
                scope,
                version,
                start,
                ..
            } => {
                let refs = s
                    .services
                    .as_ref()
                    .and_then(|s| s.references())
                    .ok_or("operation_unavailable")?;
                let page = refs.entries(scope, false, *start, Some(*version))?;
                v["kind"] = json!("select");
                v["scope"] = json!(scope.declaration);
                v["purpose"] = json!(scope.purpose);
                v["page"] = json!(page);
                v["start"] = json!(start);
            }
            WindowContent::Webhook(endpoint) => {
                v["kind"] = json!("webhook");
                v["origin"] = json!(endpoint.origin());
            }
            WindowContent::Failed => v["kind"] = json!("failed"),
            WindowContent::Transfer(t) => {
                if let Some(ticket) = &w.entry {
                    let refs = s
                        .services
                        .as_ref()
                        .and_then(|s| s.references())
                        .ok_or("operation_unavailable")?;
                    let current = refs.version()?;
                    if current.epoch != ticket.version.epoch
                        || current.revision != ticket.version.revision
                    {
                        return Err("stale_revision".into());
                    }
                    v["authorization"] = json!(true);
                }
                v["kind"] = json!("transfer");
                v["text"] = json!(t.as_str());
                v["origin"] = json!(SUMMARY_ORIGIN);
            }
            WindowContent::Sending(_, _) => {
                v["kind"] = json!("sending");
            }
            WindowContent::Result(t) => {
                v["kind"] = json!("result");
                v["text"] = json!(t.as_str());
                let scope = PermissionScope::ClipboardWrite {
                    mode: "explicit-copy".into(),
                };
                v["copy"] = json!(
                    available(&scope, s.services.as_deref()) && granted(&d, &a.action, &scope)
                );
            }
        }
        Ok(v)
    }
    pub fn respond(
        &self,
        store: &Storage,
        id: &str,
        choice: &str,
        edited: Option<Zeroizing<String>>,
    ) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        s.prune();
        let services = s.services.clone().ok_or("operation_unavailable")?;
        let w = s.windows.get_mut(id).ok_or("cancelled")?;
        let a = w.activation.clone();
        let d = Self::decisions(store, &a)?;
        match (choice, &w.content) {
            ("deny", WindowContent::Permissions { scopes, .. }) if edited.is_none() => {
                let changes: Vec<_> = scopes
                    .iter()
                    .filter(|scope| !d.scopes.contains_key(&(a.action.clone(), scope_key(scope))))
                    .map(|scope| (a.action.clone(), scope_key(scope), false))
                    .collect();
                let revision = if changes.is_empty() {
                    d.revision
                } else {
                    store.set_permission_decisions(a.session.owner.clone(), a.revision, changes)?
                };
                let scopes = scopes.clone();
                s.finish(id, "denied", Some(revision));
                if revision != d.revision {
                    s.invalidate(|old| old.session.owner.plugin == a.session.owner.plugin);
                    s.notice(&a.alive,json!({"event":"streamDockManagerPermissionsChanged","revision":revision.to_string(),"action":a.action,"scopes":scopes}));
                }
                Ok(())
            }
            ("deny", _) if edited.is_none() => {
                s.finish(id, "denied", None);
                Ok(())
            }
            (
                "allow",
                WindowContent::Permissions {
                    scopes,
                    vault_version,
                    ..
                },
            ) if edited.is_none() => {
                let mut changes = scopes
                    .iter()
                    .map(|v| (a.action.clone(), scope_key(v), true))
                    .collect();
                let mut commit = || {
                    store.set_permission_decisions(
                        a.session.owner.clone(),
                        a.revision,
                        std::mem::take(&mut changes),
                    )
                };
                let revision = match vault_version {
                    Some(v) => services
                        .references()
                        .ok_or("operation_unavailable")?
                        .approve(*v, &mut commit)?,
                    None => commit()?,
                };
                let affected = scopes.clone();
                s.finish(id, "approved", Some(revision));
                s.invalidate(|old| old.session.owner.plugin == a.session.owner.plugin);
                s.notice(&a.alive,json!({"event":"streamDockManagerPermissionsChanged","revision":revision.to_string(),"action":a.action,"scopes":affected}));
                Ok(())
            }
            ("next" | "previous", WindowContent::Select { start, .. }) if edited.is_none() => {
                let next = if choice == "next" {
                    start
                        .checked_add(32)
                        .filter(|v| *v <= 4096)
                        .ok_or("invalid_request")?
                } else {
                    start.saturating_sub(32)
                };
                if let WindowContent::Select { start, .. } = &mut w.content {
                    *start = next;
                }
                Ok(())
            }
            (
                "select",
                WindowContent::Select {
                    request,
                    scope,
                    version,
                    start,
                },
            ) => {
                let refs = services.references().ok_or("operation_unavailable")?;
                let selected = edited.filter(|v| v.len() == 73).ok_or("invalid_request")?;
                let page = refs.entries(scope, false, *start, Some(*version))?;
                if !page.entries.iter().any(|e| e.id == *selected) {
                    return Err("permission_denied".into());
                }
                let entry = refs.select(scope, &selected, *version, &|| {
                    a.live() && Instant::now() < w.expires
                })?;
                let request = request.clone();
                s.windows.remove(id);
                if a.live() {
                    s.notice(&a.alive,json!({"event":"streamDockManagerVaultSelectionResult","request_id":request,"review_id":id,"outcome":"selected","entry":entry}));
                }
                services.changed(&a.session.owner.plugin);
                services.close(id);
                Ok(())
            }
            ("copy", WindowContent::Result(text)) if edited.is_none() => {
                if !services.copy_available() {
                    return Err("operation_unavailable".into());
                }
                if !granted(
                    &d,
                    &a.action,
                    &PermissionScope::ClipboardWrite {
                        mode: "explicit-copy".into(),
                    },
                ) {
                    return Err("permission_denied".into());
                }
                services.copy(text)?;
                s.finish(id, "completed", None);
                Ok(())
            }
            ("send", WindowContent::Transfer(_) | WindowContent::Webhook(_)) => {
                let webhook = match &w.content {
                    WindowContent::Webhook(cert) => Some(cert.clone()),
                    _ => None,
                };
                let text = if webhook.is_some() && edited.is_none() {
                    Zeroizing::new(String::new())
                } else {
                    edited
                        .filter(|t| webhook.is_none() && t.len() <= TEXT_LIMIT)
                        .ok_or("invalid_request")?
                };
                let ticket = w.entry.clone();
                w.content = WindowContent::Sending(
                    if webhook.is_some() {
                        "manager-webhook"
                    } else {
                        "clipboard-summary"
                    }
                    .into(),
                    webhook
                        .as_ref()
                        .map(|e| e.origin())
                        .unwrap_or(SUMMARY_ORIGIN)
                        .into(),
                );
                services.changed(&a.session.owner.plugin);
                let deadline = w.expires;
                drop(s);
                let refs = services.references();
                let cancelled = || {
                    !a.live()
                        || Instant::now() >= deadline
                        || ticket.as_ref().is_some_and(|t| {
                            refs.and_then(|r| r.version().ok()).is_none_or(|v| {
                                v.epoch != t.version.epoch || v.revision != t.version.revision
                            })
                        })
                };
                let answer = (|| {
                    if cancelled() {
                        return Err("cancelled");
                    }
                    if let Some(cert) = &webhook {
                        services.webhook(cert, &cancelled)?;
                        return Ok(Zeroizing::new("Local webhook received the ping.".into()));
                    }
                    match ticket
                        .as_ref()
                        .map(|t| refs.ok_or("operation_unavailable")?.resolve(t))
                        .transpose()?
                    {
                        None => services.summarize(&text, &cancelled),
                        Some(RecipeValue::Authorization(key)) => {
                            let result = services.credentialed_summary(&text, &key, &cancelled)?;
                            if result.contains(key.as_str()) {
                                return Err("operation_unavailable");
                            }
                            Ok(result)
                        }
                    }
                })();
                let mut s = self.state.lock().unwrap();
                services.changed(&a.session.owner.plugin);
                s.prune();
                let w = s.windows.get_mut(id).ok_or("cancelled")?;
                Self::decisions(store, &a)?;
                if cancelled() {
                    s.finish(id, "cancelled", None);
                    return Err("cancelled".into());
                }
                match answer {
                    Ok(answer) if answer.len() <= TEXT_LIMIT => {
                        w.content = WindowContent::Result(answer);
                    }
                    _ => w.content = WindowContent::Failed,
                }
                Ok(())
            }
            _ => Err("invalid_request".into()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    action: String,
    scope: PermissionScope,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    #[serde(default)]
    certificate: Option<String>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    text: Option<String>,
    activation_token: String,
    #[serde(default)]
    entry_ref: Option<String>,
    #[serde(default)]
    scope: Option<PermissionScope>,
    #[serde(default)]
    scopes: Vec<PermissionScope>,
}

fn scope_status(
    s: &Session,
    action: &str,
    scope: &PermissionScope,
    d: &Decisions,
    svc: Option<&dyn Services>,
) -> (bool, bool) {
    if matches!(
        scope,
        PermissionScope::ReadSecret { .. }
            | PermissionScope::ReadLocation { .. }
            | PermissionScope::WriteSecret { .. }
    ) {
        let Ok(exact) = reference_scope(s, action, scope, d.revision) else {
            return (false, false);
        };
        return svc
            .and_then(Services::references)
            .map(|v| v.secret_grant(&exact))
            .unwrap_or((false, false));
    }
    (granted(d, action, scope), available(scope, svc))
}
fn rows(s: &Session, d: &Decisions, svc: Option<&dyn Services>) -> Value {
    let mut rows = Vec::new();
    for (action, g) in &s.declaration.actions {
        for scope in g.required.iter().chain(&g.optional) {
            let (granted, available) = scope_status(s, action, scope, d, svc);
            rows.push(
                json!({"action":action,"scope":scope,"granted":granted,"available":available}),
            );
        }
    }
    json!({"revision":d.revision.to_string(),"scopes":rows})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceRequest {
    action: String,
    scope: PermissionScope,
    activation_token: Option<String>,
    start: Option<usize>,
}

fn reference_scope(
    session: &Session,
    action: &str,
    scope: &PermissionScope,
    revision: u64,
) -> Result<ReferenceScope, &'static str> {
    let group = session
        .declaration
        .actions
        .get(action)
        .ok_or("not_declared")?;
    if !group.required.contains(scope) && !group.optional.contains(scope) {
        return Err("not_declared");
    }
    let owner = &session.owner;
    Ok(ReferenceScope {
        owner: vault_rpc::VaultInstallation {
            owner: owner.plugin.clone(),
            token: owner.installation.clone(),
            inventory_sha256: owner.inventory.clone(),
            storage_generation: owner.generation.clone(),
        },
        action: action.into(),
        purpose: group.purpose.clone(),
        declaration: scope.clone(),
        permission_revision: revision,
    })
}

impl Workflow {
    fn reference_request(
        s: &mut State,
        d: &Decisions,
        session: &Session,
        lease: &Arc<AtomicBool>,
        message: &Value,
        services: &dyn Services,
    ) -> Result<Value, &'static str> {
        let refs = services.references().ok_or("operation_unavailable")?;
        let q: ReferenceRequest =
            serde_json::from_value(message["payload"].clone()).map_err(|_| "invalid_request")?;
        let PermissionScope::Access { ref mode, .. } = q.scope else {
            return Err("invalid_request");
        };
        let scope = reference_scope(session, &q.action, &q.scope, d.revision)?;
        if !granted(d, &q.action, &q.scope) {
            return Err("permission_denied");
        }
        if message["method"] == "list-vault-entries" {
            if mode != "browse-metadata" || q.activation_token.is_some() {
                return Err("permission_denied");
            }
            return Ok(json!(refs.entries(
                &scope,
                true,
                q.start.unwrap_or(0),
                None
            )?));
        }
        if message["method"] != "select-vault-entry" || q.start.is_some() {
            return Err("invalid_request");
        }
        let token = q.activation_token.ok_or("activation_required")?;
        let a = s.tokens.get(&token).ok_or("activation_required")?;
        if !Arc::ptr_eq(lease, &a.alive)
            || a.action != q.action
            || a.session.owner.installation != session.owner.installation
            || a.revision != d.revision
        {
            return Err("activation_required");
        }
        if Instant::now() >= a.expires {
            return Err("activation_expired");
        }
        if s.windows.len() >= 8
            || s.windows
                .values()
                .any(|w| w.activation.session.owner.plugin == session.owner.plugin)
        {
            return Err("rate_limited");
        }
        let version = refs.version()?;
        let activation = s.tokens.remove(&token).ok_or("activation_expired")?;
        let id = uuid::Uuid::new_v4().to_string();
        s.windows.insert(
            id.clone(),
            Window {
                activation,
                entry: None,
                content: WindowContent::Select {
                    request: message["request_id"]
                        .as_str()
                        .ok_or("invalid_request")?
                        .into(),
                    scope: Box::new(scope),
                    version,
                    start: 0,
                },
                expires: Instant::now() + WINDOW_TTL,
            },
        );
        services.changed(&session.owner.plugin);
        if let Err(e) = services.open(&id) {
            s.windows.remove(&id);
            return Err(e);
        }
        Ok(json!({"review_id":id,"state":"pending","revision":d.revision.to_string()}))
    }
    fn create_notification(
        s: &mut State,
        session: &Session,
        lease: &Arc<AtomicBool>,
        decisions: &Decisions,
        payload: &Value,
        services: &dyn Services,
    ) -> Result<Value, &'static str> {
        let request: CreateNotification =
            serde_json::from_value(payload.clone()).map_err(|_| "invalid_request")?;
        if !permissions::identifier(&request.action, 256) {
            return Err("invalid_request");
        }
        let scope = notification_scope();
        if !session
            .declaration
            .actions
            .get(&request.action)
            .is_some_and(|group| group.required.contains(&scope) || group.optional.contains(&scope))
        {
            return Err("not_declared");
        }
        if !granted(decisions, &request.action, &scope) {
            return Err("permission_denied");
        }
        if !available(&scope, Some(services)) {
            return Err("operation_unavailable");
        }
        let message = notification_text(&request.message)?;
        let now = s.admit_notification(&session.owner.plugin)?;
        if !lease.load(Ordering::Acquire) {
            return Err("cancelled");
        }
        // Share the delivery budget with old completion notices. Reserve before
        // emission so any downstream failure cannot be used to flood retries.
        s.notifications
            .push_back((now, session.owner.plugin.clone()));
        services.create_plugin_notification(&PluginNotification {
            plugin: session.owner.plugin.clone(),
            level: request.level,
            message,
        })?;
        Ok(json!({"state":"created"}))
    }

    fn request(
        s: &mut State,
        store: &Storage,
        session: &Session,
        lease: &Arc<AtomicBool>,
        message: &Value,
    ) -> Result<Value, &'static str> {
        s.prune();
        let services = s.services.clone().ok_or("operation_unavailable")?;
        let d = store
            .permission_decisions(session.owner.clone())
            .map_err(|_| "operation_unavailable")?;
        if !lease.load(Ordering::Acquire) {
            return Err("cancelled");
        }
        let payload = &message["payload"];
        let protocol = message["protocol_id"].as_str();
        if protocol == Some(vault_references::PROTOCOL) {
            return Self::reference_request(s, &d, session, lease, message, &*services);
        }
        match (protocol, message["method"].as_str()) {
            (Some(PROTOCOL), Some("get-permissions")) if payload == &json!({}) => {
                return Ok(rows(session, &d, Some(&*services)));
            }
            (Some(PROTOCOL), Some("create-notification")) => {
                return Self::create_notification(s, session, lease, &d, payload, &*services);
            }
            (Some(PROTOCOL), Some("has-permission")) => {
                let q: Query =
                    serde_json::from_value(payload.clone()).map_err(|_| "invalid_request")?;
                let group = session
                    .declaration
                    .actions
                    .get(&q.action)
                    .ok_or("not_declared")?;
                if !group.required.contains(&q.scope) && !group.optional.contains(&q.scope) {
                    return Err("not_declared");
                }
                let (granted, available) =
                    scope_status(session, &q.action, &q.scope, &d, Some(&*services));
                return Ok(
                    json!({"revision":d.revision.to_string(),"granted":granted,"available":available}),
                );
            }
            (
                Some(PROTOCOL),
                Some(
                    "request-permissions"
                    | "read-clipboard-text"
                    | "show-text-result"
                    | "notify-completion",
                ),
            )
            | (
                Some(BROKER),
                Some("clipboard-summary" | "manager-webhook" | "manager-local-webhook"),
            ) => {}
            _ => return Err("unsupported"),
        }
        let req: Request =
            serde_json::from_value(payload.clone()).map_err(|_| "invalid_request")?;
        let a = s
            .tokens
            .get(&req.activation_token)
            .ok_or("activation_required")?;
        if !Arc::ptr_eq(lease, &a.alive)
            || a.session.owner.installation != session.owner.installation
        {
            return Err("activation_required");
        }
        if Instant::now() >= a.expires {
            return Err("activation_expired");
        }
        if a.revision != d.revision {
            return Err("permission_denied");
        }
        let a = s.tokens.remove(&req.activation_token).unwrap();
        let group = &session.declaration.actions[&a.action];
        let review = message["method"] == "request-permissions";
        let direct = message["method"] == "read-clipboard-text";
        let popup = message["method"] == "show-text-result";
        let notification = message["method"] == "notify-completion";
        if popup != req.text.is_some() {
            return Err("invalid_request");
        }
        let tls = message["method"] == "manager-webhook";
        let http = message["method"] == "manager-local-webhook";
        if tls != req.certificate.is_some()
            || http != req.origin.is_some()
            || req
                .origin
                .as_ref()
                .is_some_and(|v| permissions::loopback_http_address(v).is_none())
            || req
                .certificate
                .as_ref()
                .is_some_and(|c| c.is_empty() || c.len() > 4096)
            || ((tls || http) && (req.scope.is_some() || req.entry_ref.is_some()))
        {
            return Err("invalid_request");
        }
        let webhook = req
            .certificate
            .map(|certificate| WebhookEndpoint::Https { certificate })
            .or_else(|| req.origin.map(|origin| WebhookEndpoint::Http { origin }));
        let broker = protocol == Some(BROKER);
        let method = message["method"].as_str();
        let matches_recipe = |scope: &PermissionScope| matches!(scope, PermissionScope::Access{mode,recipe,..} if mode=="use-entry" && Some(recipe.as_str())==method);
        let ticket = match (req.entry_ref, req.scope) {
            (Some(id), Some(scope)) if broker => {
                let slot = id
                    .strip_prefix("slot:")
                    .is_some_and(|v| permissions::identifier(v, 128));
                if (!slot && id.len() != 73)
                    || !matches_recipe(&scope)
                    || !granted(&d, &a.action, &scope)
                {
                    return Err("permission_denied");
                }
                let references = services.references().ok_or("operation_unavailable")?;
                let ticket = RecipeEntry {
                    scope: reference_scope(session, &a.action, &scope, d.revision)?,
                    id,
                    version: references.version()?,
                };
                references.resolve(&ticket)?;
                Some(ticket)
            }
            (None, None) if !broker || !group.required.iter().any(matches_recipe) => None,
            _ => return Err("permission_denied"),
        };
        let scopes = if review {
            if req.scopes.is_empty() || req.scopes.len() > 16 {
                return Err("invalid_request");
            }
            req.scopes
        } else {
            if !req.scopes.is_empty() {
                return Err("invalid_request");
            }
            if popup {
                vec![PermissionScope::Popup {
                    mode: "text-result".into(),
                }]
            } else if notification {
                vec![PermissionScope::Notification {
                    mode: "completion-only".into(),
                }]
            } else if let Some(endpoint) = &webhook {
                vec![
                    endpoint.scope(),
                    PermissionScope::Popup {
                        mode: "text-result".into(),
                    },
                ]
            } else if direct {
                vec![PermissionScope::ClipboardRead {
                    mode: "direct".into(),
                }]
            } else {
                vec![
                    PermissionScope::ClipboardRead {
                        mode: "manager-recipe".into(),
                    },
                    PermissionScope::Popup {
                        mode: "text-result".into(),
                    },
                    PermissionScope::Https {
                        origin: SUMMARY_ORIGIN.into(),
                        recipe: "clipboard-summary".into(),
                        data_class: "clipboard.text".into(),
                    },
                ]
            }
        };
        for scope in &scopes {
            if !group.required.contains(scope) && !group.optional.contains(scope) {
                return Err("not_declared");
            }
            if !available(scope, Some(&*services)) {
                return Err("operation_unavailable");
            }
            if !review && !granted(&d, &a.action, scope) {
                return Err("permission_denied");
            }
        }
        if notification {
            let now = s.admit_notification(&session.owner.plugin)?;
            services.notify_completed(&session.owner.plugin)?;
            s.notifications
                .push_back((now, session.owner.plugin.clone()));
            return Ok(json!({"state":"completed"}));
        }
        if s.windows.len() >= 8
            || s.windows
                .values()
                .any(|w| w.activation.session.owner.plugin == session.owner.plugin)
        {
            return Err("rate_limited");
        }
        let content = if popup {
            let text = Zeroizing::new(req.text.ok_or("invalid_request")?);
            if text.len() > TEXT_LIMIT {
                return Err("too_large");
            }
            WindowContent::Result(text)
        } else if let Some(endpoint) = webhook {
            WindowContent::Webhook(endpoint)
        } else if review {
            let vault_version = scopes
                .iter()
                .any(|s| matches!(s, PermissionScope::Access { .. }))
                .then(|| {
                    services
                        .references()
                        .ok_or("operation_unavailable")?
                        .version()
                })
                .transpose()?;
            WindowContent::Permissions {
                vault_version,
                scopes,
                request: message["request_id"].as_str().unwrap().to_owned(),
            }
        } else {
            let text = services.capture(&|| !a.live() || Instant::now() >= a.expires)?;
            if !a.live() || Instant::now() >= a.expires {
                return Err("activation_expired");
            }
            if text.len() > TEXT_LIMIT {
                return Err("too_large");
            }
            if direct {
                return Ok(json!({"text":text.as_str(),"revision":d.revision.to_string()}));
            }
            WindowContent::Transfer(text)
        };
        let id = uuid::Uuid::new_v4().to_string();
        s.windows.insert(
            id.clone(),
            Window {
                activation: a,
                entry: ticket,
                content,
                expires: Instant::now() + WINDOW_TTL,
            },
        );
        services.changed(&session.owner.plugin);
        if let Err(e) = services.open(&id) {
            s.windows.remove(&id);
            return Err(e);
        }
        Ok(if review {
            json!({"review_id":id,"state":"pending","revision":d.revision.to_string()})
        } else {
            json!({"result_handle":id,"state":"pending"})
        })
    }
}

impl PluginHost {
    pub fn permission_workflow(&self) -> Arc<Workflow> {
        self.hub.lock().unwrap().workflow.clone()
    }
    pub(super) fn permission_session(&self, id: &str) -> Option<Session> {
        let info = &self.plugins.get(id)?.info;
        let declaration = info.permissions.clone()?;
        info.capabilities.as_ref()?;
        let install = self.installation_inventory(id)?;
        if self.provenance(id)?["permission_reviewed_inventory"].as_str()?
            != install.inventory_sha256
        {
            return None;
        }
        Some(Session {
            owner: PermissionOwner {
                plugin: install.owner,
                installation: install.token,
                inventory: install.inventory_sha256,
                generation: install.storage_generation,
            },
            declaration,
        })
    }
    pub fn begin_physical_permission(
        &self,
        id: &str,
        action: &str,
        device: &str,
        control: &str,
        scope: &str,
        stopped: Arc<AtomicBool>,
    ) -> Option<String> {
        let token = &self.plugins.get(id)?.token;
        let (workflow, session, lease, context) = {
            let h = self.hub.lock().unwrap();
            let session = h.permission_sessions.get(token)?.clone();
            let context = h.lookup_binding_context(id, scope, device, control)?;
            if h.context_state.visible.get(&context)?["action"] != action {
                return None;
            }
            (
                h.workflow.clone(),
                session,
                h.sessions.storage_sessions.get(token)?.clone(),
                context,
            )
        };
        let revision = self
            .storage()
            .permission_decisions(session.owner.clone())
            .ok()?
            .revision;
        workflow.start(session, lease, stopped, context, action, revision)
    }
    pub fn installed_permissions(&self, id: &str) -> Result<Value, String> {
        let session = self.permission_session(id).ok_or("operation_unavailable")?;
        let wf = self.permission_workflow();
        let s = wf.state.lock().unwrap();
        let d = self.storage().permission_decisions(session.owner.clone())?;
        let mut r = rows(&session, &d, s.services.as_deref());
        if let Some(rows) = r["scopes"].as_array_mut() {
            for row in rows {
                let Some(action) = row["action"].as_str() else {
                    continue;
                };
                let Ok(scope) = serde_json::from_value::<PermissionScope>(row["scope"].clone())
                else {
                    continue;
                };
                let key = (action.to_owned(), scope_key(&scope));
                let mut decision = match d.scopes.get(&key) {
                    Some(true) => "granted",
                    Some(false) if d.revoked.contains(&key) => "revoked",
                    Some(false) => "denied",
                    None => "undecided",
                };
                let mut active = false;
                for window in s.windows.values().filter(|w| {
                    w.activation.live()
                        && Instant::now() < w.expires
                        && w.activation.session.owner.installation == session.owner.installation
                        && w.activation.action == action
                }) {
                    match &window.content {
                        WindowContent::Permissions { scopes, .. } if scopes.contains(&scope) => {
                            decision = "pending"
                        }
                        WindowContent::Select {
                            scope: selected, ..
                        } if selected.declaration == scope => decision = "pending",
                        WindowContent::Sending(active_recipe, active_origin)
                            if matches!(&scope,PermissionScope::Https{recipe,origin,..} | PermissionScope::Http{recipe,origin,..} if recipe == active_recipe && origin == active_origin)
                                || window
                                    .entry
                                    .as_ref()
                                    .is_some_and(|e| e.scope.declaration == scope) =>
                        {
                            active = true
                        }
                        _ => {}
                    }
                }
                row["decision"] = json!(decision);
                row["active"] = json!(active);
            }
        }
        r["installation"] = json!(session.owner.installation);
        r["inventory"] = json!(session.owner.inventory);
        Ok(r)
    }
    pub fn change_permissions(
        &self,
        id: &str,
        installation: &str,
        inventory: &str,
        expected: &str,
        grants: &[permissions::ActionScope],
        allow: bool,
    ) -> Result<(), String> {
        let session = self.permission_session(id).ok_or("operation_unavailable")?;
        if session.owner.installation != installation || session.owner.inventory != inventory {
            return Err("stale_revision".into());
        }
        // Validate and build the full batch before touching storage
        if !(1..=128 * 64).contains(&grants.len()) {
            return Err("invalid_request".into());
        }
        let mut by_action = std::collections::BTreeMap::<_, Vec<_>>::new();
        let mut changes = Vec::with_capacity(grants.len());
        let mut needs_vault = false;
        let (wf, lease) = {
            let h = self.hub.lock().unwrap();
            (
                h.workflow.clone(),
                h.sessions
                    .storage_sessions
                    .get(&self.plugins[id].token)
                    .cloned(),
            )
        };
        let mut s = wf.state.lock().unwrap();
        for grant in grants {
            let group = session
                .declaration
                .actions
                .get(&grant.action)
                .ok_or("not_declared")?;
            let declared = if grant.required {
                &group.required
            } else {
                &group.optional
            };
            let scope = &grant.scope;
            if !declared.contains(scope) {
                return Err("not_declared".into());
            }
            if matches!(
                scope,
                PermissionScope::ReadSecret { .. }
                    | PermissionScope::ReadLocation { .. }
                    | PermissionScope::WriteSecret { .. }
            ) {
                return Err("permission_denied".into());
            }
            if allow && !available(scope, s.services.as_deref()) {
                return Err("operation_unavailable".into());
            }
            let scopes = by_action.entry(grant.action.as_str()).or_default();
            if scopes.contains(&scope) {
                return Err("invalid_request".into());
            }
            scopes.push(scope);
            needs_vault |= allow && matches!(scope, PermissionScope::Access { .. });
            changes.push((grant.action.clone(), scope_key(scope), allow));
        }
        let expected = expected
            .parse::<u64>()
            .ok()
            .filter(|v| v.to_string() == expected)
            .ok_or("invalid_request")?;
        let mut pending = Some(changes);
        let mut commit = || {
            self.storage().set_permission_decisions(
                session.owner.clone(),
                expected,
                pending.take().ok_or("invalid_request")?,
            )
        };
        let revision = if needs_vault {
            let vault = s
                .services
                .as_ref()
                .and_then(|s| s.references())
                .ok_or("operation_unavailable")?;
            vault.approve(vault.version()?, &mut commit)?
        } else {
            commit()?
        };
        s.invalidate(|a| a.session.owner.plugin == id);
        if let Some(lease) = lease {
            for (action, scopes) in by_action {
                s.notice(&lease,json!({"event":"streamDockManagerPermissionsChanged","action":action,"revision":revision.to_string(),"scopes":scopes}));
            }
        }
        Ok(())
    }
    pub(super) fn ordinary_required_ready(
        &self,
        id: &str,
        action: &str,
        scopes: &[&PermissionScope],
    ) -> bool {
        let Some(plugin) = self.plugins.get(id) else {
            return false;
        };
        let (wf, session) = {
            let h = self.hub.lock().unwrap();
            (
                h.workflow.clone(),
                h.permission_sessions.get(&plugin.token).cloned(),
            )
        };
        // A macro checks grants before creating the demand that launches a dormant plugin.
        // Active plugins still need their live session; only dormant checks use the reviewed install.
        let session = session.or_else(|| {
            (!matches!(
                plugin.info.runtime.state,
                RuntimeState::Starting | RuntimeState::Running
            ))
            .then(|| self.permission_session(id))
            .flatten()
        });
        session.is_some_and(|session| {
            let s = wf.state.lock().unwrap();
            self.storage()
                .permission_decisions(session.owner)
                .is_ok_and(|d| {
                    scopes.iter().all(|scope| {
                        available(scope, s.services.as_deref()) && granted(&d, action, scope)
                    })
                })
        })
    }
}

pub(super) fn dispatch(
    workflow: &Workflow,
    store: &Storage,
    session: Option<&Session>,
    lease: Option<&Arc<AtomicBool>>,
    (message, length): (&Value, usize),
    admitted: Result<(), &'static str>,
    send: impl FnOnce(Value) -> bool,
) -> bool {
    // Serialize the direct response with UI revocation, context retirement and room close.
    let mut state = workflow.state.lock().unwrap();
    let result = (|| {
        admitted?;
        if length
            > (match message["method"].as_str() {
                Some("show-text-result") => 6 * TEXT_LIMIT + 1024,
                Some("create-notification") => 6 * NOTIFICATION_MESSAGE_LIMIT + 1024,
                _ => 16 * 1024,
            })
            || !message.as_object().is_some_and(|m| {
                m.len() == 5
                    && ["event", "protocol_id", "request_id", "method", "payload"]
                        .iter()
                        .all(|k| m.contains_key(*k))
            })
        {
            return Err("invalid_request");
        }
        Workflow::request(
            &mut state,
            store,
            session.ok_or("permission_denied")?,
            lease.ok_or("permission_denied")?,
            message,
        )
    })();
    let mut response = json!({"event":"streamDockManagerRpcResponse","protocol_id":message["protocol_id"],"request_id":message["request_id"].as_str().filter(|v|uuid::Uuid::parse_str(v).is_ok_and(|u|u.to_string()==*v)).unwrap_or("")});
    match result {
        Ok(value) => {
            if lease.is_none_or(|l| !l.load(Ordering::Acquire)) {
                return false;
            }
            response["result"] = value;
        }
        Err(code) => {
            response["error"] =
                json!({"code":code,"message":"Manager permission request unavailable"})
        }
    };
    send(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fixture {
        captures: std::sync::atomic::AtomicUsize,
        sent: Mutex<Vec<String>>,
        copied: Mutex<Vec<String>>,
        emitted: Mutex<Vec<PluginNotification>>,
    }
    impl Services for Fixture {
        fn clipboard_available(&self) -> bool {
            true
        }
        fn plugin_notifications_available(&self) -> bool {
            true
        }
        fn create_plugin_notification(&self, n: &PluginNotification) -> Result<(), &'static str> {
            self.emitted.lock().unwrap().push(n.clone());
            Ok(())
        }
        fn copy_available(&self) -> bool {
            true
        }
        fn capture(&self, cancelled: &dyn Fn() -> bool) -> Result<Zeroizing<String>, &'static str> {
            assert!(!cancelled());
            self.captures.fetch_add(1, Ordering::SeqCst);
            Ok(Zeroizing::new("synthetic clipboard αβ".into()))
        }
        fn copy(&self, text: &str) -> Result<(), &'static str> {
            self.copied.lock().unwrap().push(text.into());
            Ok(())
        }
        fn summarize(
            &self,
            text: &str,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Zeroizing<String>, &'static str> {
            assert!(!cancelled());
            self.sent.lock().unwrap().push(text.into());
            Ok(Zeroizing::new("synthetic answer".into()))
        }
        fn open(&self, _: &str) -> Result<(), &'static str> {
            Ok(())
        }
        fn close(&self, _: &str) {}
    }
    #[test]
    fn notification_requests_need_exact_live_grant_and_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open_staging(root.path().join("data")).unwrap();
        let action = "fixture.plugin.notice";
        let second = "fixture.plugin.finished";
        let scope = notification_scope();
        let optional = json!({"purpose":"A user-approved background alert","optional":[scope]});
        let required_scope =
            json!({"purpose":"A user-approved background alert","required":[scope]});
        let session = Session {
            owner: PermissionOwner {
                plugin: "fixture.plugin".into(),
                installation: "new".into(),
                inventory: "checked".into(),
                generation: store.stats().unwrap().storage_generation,
            },
            declaration: serde_json::from_value(json!({
                "version":1,"actions":{action:required_scope, second:optional}
            }))
            .unwrap(),
        };
        let other_install = Session {
            owner: PermissionOwner {
                installation: "different".into(),
                ..session.owner.clone()
            },
            declaration: session.declaration.clone(),
        };
        let mut required = json!({
            "Actions":[{"UUID":action}],
            "StreamDockManager":{"Permissions":{"version":1,"actions":{
                action:{"purpose":"fixture","required":[scope]}
            }}}
        });
        let declaration = permissions::declarations(&required).unwrap().unwrap();
        assert_eq!(declaration.actions[action].required, [scope.clone()]);
        assert!(Session {
            owner: session.owner.clone(),
            declaration,
        }
        .declares_plugin_notifications());
        required["StreamDockManager"]["Permissions"]["actions"][action] = optional;
        assert!(permissions::declarations(&required).unwrap().is_some());

        let flow = Workflow::default();
        let fixture = Arc::new(Fixture::default());
        let lease = Arc::new(AtomicBool::new(true));
        let request = |session: Option<&Session>, payload: Value| {
            let message = json!({"event":"streamDockManagerRpcRequest","protocol_id":PROTOCOL,
                "request_id":uuid::Uuid::new_v4().to_string(),"method":"create-notification","payload":payload});
            let mut response = Value::Null;
            dispatch(
                &flow,
                &store,
                session,
                Some(&lease),
                (&message, message.to_string().len()),
                Ok(()),
                |reply| {
                    response = reply;
                    true
                },
            );
            response
        };
        let payload = |action: &str| json!({"action":action,"level":"warning","message":"  Fixture\n alert  "});
        let code = |payload: Value| request(Some(&session), payload)["error"]["code"].clone();

        assert_eq!(
            request(None, payload(action))["error"]["code"],
            "permission_denied"
        );
        // A desktop may advertise the protocol before its bridge is attached,
        // but no actual notification can be created in that startup window.
        assert_eq!(code(payload(action)), "operation_unavailable");
        flow.attach(fixture.clone());
        assert_eq!(code(payload("foreign.plugin.notice")), "not_declared");
        assert_eq!(code(payload(action)), "permission_denied");
        let revision = store
            .set_permission_decisions(
                session.owner.clone(),
                0,
                vec![(action.into(), scope_key(&scope), true)],
            )
            .unwrap();
        assert_eq!(
            request(Some(&other_install), payload(action))["error"]["code"],
            "permission_denied"
        );
        assert_eq!(code(payload(second)), "permission_denied");
        assert_eq!(
            code(json!({"action":action,"level":"error","message":"Hello","plugin":"Spoof"})),
            "invalid_request"
        );
        assert_eq!(
            code(json!({"action":action,"level":"error","message":" \n\t "})),
            "invalid_request"
        );
        assert_eq!(
            code(json!({"action":action,"level":"debug","message":"Hello"})),
            "invalid_request"
        );
        assert_eq!(
            code(json!({"action":action,"level":"error","message":"\u{202e}Hello"})),
            "invalid_request"
        );
        assert_eq!(
            code(json!({"action":action,"level":"error","message":"é".repeat(257)})),
            "too_large"
        );

        assert_eq!(
            request(Some(&session), payload(action))["result"]["state"],
            "created"
        );
        assert_eq!(code(payload(action)), "rate_limited");
        let revision = store
            .set_permission_decisions(
                session.owner.clone(),
                revision,
                vec![(action.into(), scope_key(&scope), false)],
            )
            .unwrap();
        assert!(revision > 0);
        assert_eq!(code(payload(action)), "permission_denied");
        lease.store(false, Ordering::Release);
        assert_eq!(code(payload(action)), "cancelled");
        assert_eq!(
            *fixture.emitted.lock().unwrap(),
            [PluginNotification {
                plugin: session.owner.plugin,
                level: PluginNotificationLevel::Warning,
                message: "Fixture alert".into()
            }]
        );

        let mut state = flow.state.lock().unwrap();
        for id in 1..NOTIFICATION_GLOBAL_LIMIT {
            let owner = format!("distinct-plugin-{id}");
            let now = state.admit_notification(&owner).unwrap();
            state.notifications.push_back((now, owner));
        }
        assert_eq!(
            state.admit_notification("extra-plugin"),
            Err("rate_limited")
        );
        for item in &mut state.notifications {
            item.0 -= NOTIFICATION_WINDOW;
        }
        assert!(state.admit_notification("extra-plugin").is_ok());
    }

    #[test]
    fn installed_permission_batch_changes_only_matching_actions_at_one_revision() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::host(&root.path().join("plugins"));
        let src = root.path().join("fixture.batch.sdPlugin");
        std::fs::create_dir_all(&src).unwrap();
        let id = "fixture.batch";
        let first = "fixture.batch.first";
        let second = "fixture.batch.second";
        let other = "fixture.batch.other";
        let popup = json!({"id":"ui.popup","mode":"text-result"});
        let mut manifest = json!({
            "UUID":id,"Name":"Batch","Version":"1.0","SDKVersion":1,"CodePath":"fixture",
            "StreamDockManager":{"ManifestDialect":"vsd","Permissions":{"version":1,"actions":{
                first:{"purpose":"Show results","optional":[popup]},
                second:{"purpose":"Show results","optional":[popup]},
                other:{"purpose":"Notify","required":[notification_scope()]}
            }}},
            "Actions":[
                {"UUID":first,"Name":"First","Controllers":["Keypad"]},
                {"UUID":second,"Name":"Second","Controllers":["Keypad"]},
                {"UUID":other,"Name":"Other","Controllers":["Keypad"]}
            ]
        });
        lifecycle_tests::current_descriptor(&src, &mut manifest);
        host.permission_workflow()
            .attach(Arc::new(Fixture::default()));
        let mut candidate = PluginHost::prepare_install(&host.root, &src).unwrap();
        let required: Vec<_> = host
            .reviewable_services(&candidate)
            .into_iter()
            .filter(|grant| grant.required)
            .collect();
        assert_eq!(required.len(), 1);
        host.stage_service_grants(&mut candidate, &host.service_review(id).unwrap(), &required)
            .unwrap();
        host.register_prepared(candidate).unwrap();
        host.plugins.get_mut(id).unwrap().info.enabled = true;
        let initial = host.installed_permissions(id).unwrap();
        // A user may revoke required access after a successful install
        host.change_permissions(
            id,
            initial["installation"].as_str().unwrap(),
            initial["inventory"].as_str().unwrap(),
            initial["revision"].as_str().unwrap(),
            &required,
            false,
        )
        .unwrap();
        let before = host.installed_permissions(id).unwrap();
        assert!(host.check_action_permissions(id, other).is_err());
        assert!(host.begin_manual_action(id, other).is_err());
        let scope = serde_json::from_value::<PermissionScope>(popup).unwrap();
        let grant = |action: &str, scope: PermissionScope| permissions::ActionScope {
            action: action.into(),
            required: false,
            scope,
        };
        let pair = [grant(first, scope.clone()), grant(second, scope.clone())];
        let mixed = [
            pair[0].clone(),
            permissions::ActionScope {
                required: true,
                ..grant(other, notification_scope())
            },
        ];
        let update = |revision: &Value, grants: &[permissions::ActionScope], allow| {
            host.change_permissions(
                id,
                before["installation"].as_str().unwrap(),
                before["inventory"].as_str().unwrap(),
                revision.as_str().unwrap(),
                grants,
                allow,
            )
        };
        assert_eq!(
            update(
                &before["revision"],
                &[pair[0].clone(), grant(other, scope)],
                true
            ),
            Err("not_declared".into())
        );
        assert_eq!(host.installed_permissions(id).unwrap(), before);
        update(&before["revision"], &pair, true).unwrap();
        let after = host.installed_permissions(id).unwrap();
        let granted = |status: &Value, action: &str| {
            status["scopes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["action"] == action)
                .unwrap()["granted"]
                == true
        };
        assert!(granted(&after, first) && granted(&after, second));
        assert!(!granted(&after, other));
        assert_eq!(
            update(&before["revision"], &pair, false),
            Err("stale_revision".into())
        );
        assert_eq!(host.installed_permissions(id).unwrap(), after);
        update(&after["revision"], &mixed, true).unwrap();
        let expanded = host.installed_permissions(id).unwrap();
        assert!(granted(&expanded, first) && granted(&expanded, other));
        assert!(host.check_action_permissions(id, other).is_ok());
        assert!(host.begin_manual_action(id, other).is_ok());
        update(&expanded["revision"], &mixed, false).unwrap();
        let revoked = host.installed_permissions(id).unwrap();
        assert!(!granted(&revoked, first) && !granted(&revoked, other));
        assert!(granted(&revoked, second));
        assert!(host.check_action_permissions(id, other).is_err());
        assert!(host.begin_manual_action(id, other).is_err());
    }

    #[test]
    fn reviewed_install_and_dispatch_bind_the_receipt_to_the_live_hardware_event() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("plugins");
        let mut host = lifecycle_tests::host(&dir);
        let dir = host.root.clone();
        let src = root.path().join("fixture.demo.sdPlugin");
        std::fs::create_dir_all(&src).unwrap();
        let id = "fixture.demo";
        let action = "fixture.demo.press";
        let mut manifest = json!({"UUID":id,"Name":"Demo","Version":"1.0","SDKVersion":1,"CodePath":"fixture",
            "StreamDockManager":{"ManifestDialect":"vsd","Permissions":{"version":1,"actions":{action:{"purpose":"demo","required":[{"id":"ui.popup","mode":"text-result"}]}}}},
            "Actions":[{"UUID":action,"Name":"Press","Controllers":["Keypad"]}]});
        lifecycle_tests::current_descriptor(&src, &mut manifest);
        let flow = host.permission_workflow();
        flow.attach(Arc::new(Fixture::default()));
        let mut candidate = PluginHost::prepare_install(&dir, &src).unwrap();
        let grants = host.reviewable_services(&candidate);
        assert_eq!(grants.len(), 1);
        let review = host.service_review(id).unwrap();
        let missing = "Approve all required Manager permissions before installing";
        assert_eq!(
            host.stage_service_grants(&mut candidate, &review, &[]),
            Err(missing.into())
        );
        assert_eq!(
            host.register_prepared(PluginHost::prepare_install(&dir, &src).unwrap())
                .unwrap_err(),
            missing
        );
        assert!(host.list().iter().all(|plugin| plugin.id != id));
        host.stage_service_grants(&mut candidate, &review, &grants)
            .unwrap();
        host.register_prepared(candidate).unwrap();
        let current = host.installed_permissions(id).unwrap();
        assert_eq!(current["scopes"][0]["granted"], true);
        let prepare = || PluginHost::prepare_install(&dir, &src).unwrap();
        let mut candidate = prepare();
        let review = host.service_review(id).unwrap();
        assert!(host
            .stage_service_grants(&mut candidate, &review, &[])
            .is_err());
        host.stage_service_grants(&mut candidate, &review, &grants)
            .unwrap();
        host.change_permissions(
            id,
            current["installation"].as_str().unwrap(),
            current["inventory"].as_str().unwrap(),
            current["revision"].as_str().unwrap(),
            &grants,
            false,
        )
        .unwrap();
        assert!(host.replace_prepared(candidate).is_err());
        let restored = host.installed_permissions(id).unwrap();
        assert_eq!(restored["installation"], current["installation"]);
        assert_eq!(restored["scopes"][0]["granted"], false);
        host.change_permissions(
            id,
            restored["installation"].as_str().unwrap(),
            restored["inventory"].as_str().unwrap(),
            restored["revision"].as_str().unwrap(),
            &grants,
            true,
        )
        .unwrap();
        let session = host.permission_session(id).unwrap();
        let token = "socket".to_string();
        let alive = Arc::new(AtomicBool::new(true));
        let stopped = Arc::new(AtomicBool::new(false));
        let context;
        {
            let plugin = host.plugins.get_mut(id).unwrap();
            plugin.token = token.clone();
            plugin.info.enabled = true;
            plugin.info.runtime.state = RuntimeState::Running;
            let mut h = host.hub.lock().unwrap();
            h.permission_sessions.insert(token.clone(), session);
            h.sessions
                .storage_sessions
                .insert(token.clone(), alive.clone());
            context = h.binding_context(id, "profile:page", "dock", "button");
            h.context_state.visible.insert(context.clone(),json!({"event":"willAppear","action":action,"context":context,"streamDockManager":{"control_id":"button"}}));
        }
        let press = host
            .begin_physical_permission(
                id,
                action,
                "dock",
                "button",
                "profile:page",
                stopped.clone(),
            )
            .unwrap();
        let input = hardware_core::Input::Pulse;
        host.dispatch_with_state(
            id,
            action,
            "dock",
            "button",
            "profile:page",
            &input,
            &json!({}),
            None,
        )
        .unwrap();
        host.dispatch_with_activation(
            id,
            action,
            "dock",
            "button",
            "profile:page",
            &input,
            &json!({}),
            None,
            Some(&press),
        )
        .unwrap();
        let mut h = host.hub.lock().unwrap();
        let messages = h.sessions.queues.get_mut(id).unwrap().take_batch().unwrap();
        let marked: Vec<_> = messages
            .iter()
            .filter(|m| m.value["streamDockManager"]["activation_token"] == press)
            .collect();
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0].value["event"], "keyDown");
        assert!(h.context_state.visible[&context]["streamDockManager"]
            .get("activation_token")
            .is_none());
        stopped.store(true, Ordering::Release);
        assert!(!flow.deliverable(&press, Some(&alive), &context, action));
    }

    #[test]
    fn staged_review_needs_the_current_database_even_without_sql_grants() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::host(&root.path().join("plugins"));
        let dir = host.root.clone();
        let src = root.path().join("fixture.demo.sdPlugin");
        std::fs::create_dir_all(&src).unwrap();
        let id = "fixture.demo";
        let action = "fixture.demo.press";
        let writer = json!({"id":"vault.write-secret","field":"credential","entry_kind":"api-key","purpose":"Store key"});
        let mut manifest = json!({"UUID":id,"Name":"Demo","Version":"1.0","SDKVersion":1,"CodePath":"fixture",
            "StreamDockManager":{"ManifestDialect":"vsd","Permissions":{"version":1,"actions":{action:{"purpose":"demo","optional":[writer,{"id":"ui.popup","mode":"text-result"}]}}}},
            "Actions":[{"UUID":action,"Name":"Press","Controllers":["Keypad"]}]});
        lifecycle_tests::current_descriptor(&src, &mut manifest);
        host.permission_workflow()
            .attach(Arc::new(Fixture::default()));
        let store = host.storage().clone();
        let prepare = || PluginHost::prepare_install(&dir, &src).unwrap();
        let review = host.service_review(id).unwrap();
        let saved = store.backup(root.path().join("restore.sqlite3")).unwrap();
        store
            .maintenance()
            .unwrap()
            .restore(saved.path, |_| Ok(()))
            .unwrap();
        assert!(host
            .stage_service_grants(&mut prepare(), &review, &[])
            .is_err());

        // A malformed inventory must not affect a review or its final transaction.
        let inventory = stream_dock_manager_storage::recovery_directory(store.root());
        std::fs::create_dir(inventory.join("unreadable.sqlite3")).unwrap();
        assert!(store.stats().is_err());
        for selection in [0, 1, 2] {
            let mut candidate = prepare();
            let review = host.service_review(id).unwrap();
            let grants = host.reviewable_services(&candidate);
            let selected: Vec<_> = grants
                .into_iter()
                .filter(|s| match selection {
                    1 => matches!(s.scope, PermissionScope::WriteSecret { .. }),
                    2 => !matches!(s.scope, PermissionScope::WriteSecret { .. }),
                    _ => false,
                })
                .collect();
            assert_eq!(selected.len(), usize::from(selection != 0));
            host.stage_service_grants(&mut candidate, &review, &selected)
                .unwrap();
            store
                .transaction(|tx| {
                    tx.execute(
                        "UPDATE database_metadata SET value=?1 WHERE key='storage_generation'",
                        [uuid::Uuid::new_v4().to_string()],
                    )
                    .map_err(|e| e.to_string())?;
                    Ok(())
                })
                .unwrap();
            assert!(host
                .register_prepared(candidate)
                .unwrap_err()
                .contains("changed during permission review"));
            assert!(host.current_vault_installation_token(id).is_none());
            assert!(!dir.join(id).exists());
            assert!(!store.run(|c| c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='plugin_permissions_v1')", [], |r| r.get::<_,bool>(0)).map_err(|e|e.to_string())).unwrap());
        }
        for replace in [false, true] {
            let mut candidate = prepare();
            let review = host.service_review(id).unwrap();
            let grants = host.reviewable_services(&candidate);
            host.stage_service_grants(&mut candidate, &review, &grants)
                .unwrap();
            if replace {
                host.replace_prepared(candidate).unwrap();
            } else {
                host.register_prepared(candidate).unwrap();
            }
            assert!(host.current_vault_installation_token(id).is_some());
        }
    }

    #[test]
    fn review_and_transfer_require_separate_physical_activations() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("data")).unwrap();
        let scopes = json!([{"id":"clipboard.read-text","mode":"manager-recipe"},{"id":"ui.popup","mode":"text-result"},{"id":"network.https","recipe":"clipboard-summary","origin":SUMMARY_ORIGIN,"data_class":"clipboard.text"},{"id":"clipboard.write-text","mode":"explicit-copy"}]);
        let session=Session{owner:PermissionOwner{plugin:"test.package".into(),installation:"one".into(),inventory:"digest".into(),generation:store.stats().unwrap().storage_generation},declaration:serde_json::from_value(json!({"version":1,"actions":{"test.action":{"purpose":"fixture","optional":scopes}}})).unwrap()};
        let flow = Workflow::default();
        let fixture = Arc::new(Fixture::default());
        flow.attach(fixture.clone());
        let lease = Arc::new(AtomicBool::new(true));
        let token = || {
            flow.start(
                session.clone(),
                lease.clone(),
                Arc::new(AtomicBool::new(false)),
                "context".into(),
                "test.action",
                store
                    .permission_decisions(session.owner.clone())
                    .unwrap()
                    .revision,
            )
            .unwrap()
        };
        let call = |connection: &Arc<AtomicBool>, protocol: &str, method: &str, payload: Value| {
            let msg = json!({"event":"streamDockManagerRpcRequest","protocol_id":protocol,"request_id":uuid::Uuid::new_v4().to_string(),"method":method,"payload":payload});
            let mut out = Value::Null;
            dispatch(
                &flow,
                &store,
                Some(&session),
                Some(connection),
                (&msg, msg.to_string().len()),
                Ok(()),
                |v| {
                    out = v;
                    true
                },
            );
            out
        };
        assert_eq!(
            call(
                &lease,
                PROTOCOL,
                "has-permission",
                json!({"action":"test.action","scope":scopes[0]})
            )["result"]["granted"],
            false
        );
        assert_eq!(
            call(
                &lease,
                BROKER,
                "clipboard-summary",
                json!({"activation_token":token()})
            )["error"]["code"],
            "permission_denied"
        );
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 0);
        let expired = token();
        flow.state
            .lock()
            .unwrap()
            .tokens
            .get_mut(&expired)
            .unwrap()
            .expires = Instant::now() - Duration::from_millis(1);
        assert_eq!(
            call(
                &lease,
                PROTOCOL,
                "request-permissions",
                json!({"activation_token":expired,"scopes":scopes})
            )["error"]["code"],
            "activation_expired"
        );
        let review_token = token();
        let req = json!({"activation_token":review_token,"scopes":scopes});
        assert_eq!(
            call(
                &Arc::new(AtomicBool::new(true)),
                PROTOCOL,
                "request-permissions",
                req.clone()
            )["error"]["code"],
            "activation_required"
        );
        let ack = call(&lease, PROTOCOL, "request-permissions", req);
        let id = ack["result"]["review_id"].as_str().unwrap();
        assert_eq!(flow.view(&store, id).unwrap()["scopes"], scopes);
        flow.respond(&store, id, "allow", None).unwrap();
        assert!(flow.respond(&store, id, "allow", None).is_err());
        let notices = flow.notices(&lease);
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0]["request_id"], ack["request_id"]);
        assert_eq!(
            call(
                &lease,
                BROKER,
                "clipboard-summary",
                json!({"activation_token":review_token})
            )["error"]["code"],
            "activation_required"
        );
        let press = token();
        let ack = call(
            &lease,
            BROKER,
            "clipboard-summary",
            json!({"activation_token":press}),
        );
        let id = ack["result"]["result_handle"].as_str().unwrap();
        assert!(!ack.to_string().contains("synthetic"));
        assert_eq!(fixture.captures.load(Ordering::SeqCst), 1);
        assert!(fixture.sent.lock().unwrap().is_empty());
        assert!(fixture.copied.lock().unwrap().is_empty());
        assert_eq!(
            flow.view(&store, id).unwrap()["text"],
            "synthetic clipboard αβ"
        );
        assert_eq!(
            call(
                &lease,
                BROKER,
                "clipboard-summary",
                json!({"activation_token":press})
            )["error"]["code"],
            "activation_required"
        );
        flow.respond(&store, id, "send", Some(Zeroizing::new("edited β".into())))
            .unwrap();
        assert_eq!(&*fixture.sent.lock().unwrap(), &["edited β"]);
        assert_eq!(flow.view(&store, id).unwrap()["text"], "synthetic answer");
        assert!(fixture.copied.lock().unwrap().is_empty());
        flow.respond(&store, id, "copy", None).unwrap();
        assert_eq!(&*fixture.copied.lock().unwrap(), &["synthetic answer"]);
        assert!(flow.view(&store, id).is_err());
        let revoked = token();
        flow.invalidate_context("context");
        assert_eq!(
            call(
                &lease,
                BROKER,
                "clipboard-summary",
                json!({"activation_token":revoked})
            )["error"]["code"],
            "activation_required"
        );
        let cancelled = call(
            &lease,
            BROKER,
            "clipboard-summary",
            json!({"activation_token":token()}),
        );
        lease.store(false, Ordering::Release);
        assert!(flow
            .view(
                &store,
                cancelled["result"]["result_handle"].as_str().unwrap()
            )
            .is_err());
        assert_eq!(fixture.sent.lock().unwrap().len(), 1);
    }
}

/// An install review remembers the previous installation and grant revision independently
/// of the candidate's new identity. The SQLite commit compares the old revision again.
#[derive(Clone)]
pub struct InstallReview {
    pub prior_token: Option<String>,
    prior: Option<(PermissionOwner, u64)>,
    generation: String,
}

impl PluginHost {
    pub fn service_review(&self, id: &str) -> Result<InstallReview, String> {
        let prior = self
            .permission_session(id)
            .map(|s| {
                let rev = self
                    .storage()
                    .permission_decisions(s.owner.clone())?
                    .revision;
                Ok::<_, String>((s.owner, rev))
            })
            .transpose()?;
        Ok(InstallReview {
            prior_token: self.current_vault_installation_token(id),
            prior,
            generation: self.storage().persisted_generation()?,
        })
    }
    pub fn reviewable_services(&self, prepared: &PreparedPlugin) -> Vec<permissions::ActionScope> {
        if !Self::prepared_info(prepared)
            .is_ok_and(|p| p.kind != "browser" && p.capabilities.is_some())
        {
            return Vec::new();
        }
        let manifest = &prepared.manifest;
        let flow = self.permission_workflow();
        let state = flow.state.lock().unwrap();
        manifest
            .permissions
            .iter()
            .flat_map(|p| p.actions.iter())
            .flat_map(|(action, g)| {
                g.required
                    .iter()
                    .map(|s| (true, s))
                    .chain(g.optional.iter().map(|s| (false, s)))
                    .filter(|(_, scope)| {
                        available(scope, state.services.as_deref())
                            || matches!(scope, PermissionScope::WriteSecret { .. })
                    })
                    .map(|(required, scope)| permissions::ActionScope {
                        action: action.clone(),
                        required,
                        scope: scope.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
    pub fn stage_service_grants(
        &self,
        p: &mut PreparedPlugin,
        review: &InstallReview,
        selected: &[permissions::ActionScope],
    ) -> Result<(), String> {
        if self.current_vault_installation_token(&p.id) != review.prior_token
            || review.generation != self.storage().persisted_generation()?
        {
            return Err("Installation changed during permission review".into());
        }
        let offered: std::collections::BTreeSet<_> =
            self.reviewable_services(p).into_iter().collect();
        if selected.len() > offered.len() || selected.iter().any(|s| !offered.contains(s)) {
            return Err("Invalid permission selection".into());
        }
        let unique: std::collections::BTreeSet<_> = selected.iter().cloned().collect();
        if unique.len() != selected.len() {
            return Err("Duplicate permission selection".into());
        }
        if p.manifest.missing_required_services(selected) {
            return Err("Approve all required Manager permissions before installing".into());
        }
        p.service_review = Some((review.clone(), selected.to_vec()));
        Ok(())
    }
}

impl InstallReview {
    pub(super) fn commit(
        self,
        tx: &stream_dock_manager_storage::rusqlite::Transaction<'_>,
        id: String,
        token: String,
        inventory: String,
        selected: Vec<permissions::ActionScope>,
    ) -> Result<(), String> {
        use stream_dock_manager_storage::permissions::{verify_revision, write_decisions};
        if stream_dock_manager_storage::persisted_generation(tx)? != self.generation {
            return Err("Installation changed during permission review".into());
        }
        if let Some((owner, rev)) = self.prior {
            verify_revision(tx, &owner, rev)?;
        }
        let selected: Vec<_> = selected
            .into_iter()
            .filter(|s| !matches!(s.scope, PermissionScope::WriteSecret { .. }))
            .collect();
        if !selected.is_empty() {
            let owner = PermissionOwner {
                plugin: id,
                installation: token,
                inventory,
                generation: self.generation,
            };
            let changes = selected
                .into_iter()
                .map(|s| (s.action, scope_key(&s.scope), true))
                .collect();
            write_decisions(tx, &owner, 0, changes)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "vault_reference_tests.rs"]
mod reference_tests;
