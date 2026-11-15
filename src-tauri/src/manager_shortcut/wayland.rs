use super::*;
use ashpd::{
    desktop::global_shortcuts::{GlobalShortcuts, NewShortcut, Shortcut},
    zbus::{self, fdo::DBusProxy},
};
use futures_util::StreamExt;

const ID: &str = "show-manager";
const PORTAL: &str = "org.freedesktop.portal.Desktop";
const REGISTRY: &str = "org.freedesktop.host.portal.Registry";
const UNAVAILABLE: &str =
    "The desktop shortcut portal is unavailable. You can bind stream-dock-manager --show in your desktop settings.";

// Build the report for this operation
fn report(app: &AppHandle, receiver: &tokio::sync::watch::Receiver<bool>, message: &str) {
    let state = app.state::<ShortcutStateStore>();
    let current = state.portal.lock().unwrap();
    if current
        .as_ref()
        .is_some_and(|sender| sender.subscribe().same_channel(receiver))
    {
        status(app, message);
    }
}

// Show binding
fn show_binding(
    app: &AppHandle,
    receiver: &tokio::sync::watch::Receiver<bool>,
    shortcuts: &[Shortcut],
) {
    let trigger = shortcuts
        .iter()
        .find(|shortcut| shortcut.id() == ID)
        .map(Shortcut::trigger_description)
        .filter(|trigger| !trigger.is_empty());
    report(
        app,
        receiver,
        trigger.unwrap_or("No shortcut was assigned by the desktop."),
    );
}

// Handle an unavailable Wayland shortcut registry
fn registry_unavailable(error: &ashpd::Error) -> bool {
    match error {
        ashpd::Error::PortalNotFound(interface) => interface.as_str() == REGISTRY,
        ashpd::Error::Zbus(zbus::Error::MethodError(name, _, _)) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.UnknownMethod"
                | "org.freedesktop.DBus.Error.UnknownInterface"
        ),
        ashpd::Error::Zbus(zbus::Error::FDO(error)) => matches!(
            error.as_ref(),
            zbus::fdo::Error::UnknownMethod(_) | zbus::fdo::Error::UnknownInterface(_)
        ),
        _ => false,
    }
}

// Bind for the current operation
async fn bind(
    app: &AppHandle,
    connection: zbus::Connection,
    cancelled: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(), ashpd::Error> {
    let registered = ashpd::register_host_app_with_connection(
        connection.clone(),
        "app.streamdockmanager.manager"
            .parse()
            .expect("valid portal app ID"),
    )
    .await;
    // The Global Shortcuts interface predates the host registry. Older portals
    // infer the application identity and can still provide a shortcut.
    if let Err(error) = registered {
        if !registry_unavailable(&error) {
            return Err(error);
        }
    }
    let proxy = GlobalShortcuts::with_connection(connection).await?;
    let session = proxy.create_session(Default::default()).await?;
    let path = serde_json::to_value(&session).expect("session object path serializes");
    let active_worker = cancelled.clone();
    let operation = async {
        let mut activated = proxy.receive_activated().await?;
        let mut changed = proxy.receive_shortcuts_changed().await?;
        let mut closed = session.receive_closed().await?;
        let request = proxy
            .bind_shortcuts(
                &session,
                &[NewShortcut::new(ID, "Show Stream Dock Manager")
                    .preferred_trigger("CTRL+ALT+SHIFT+s")],
                None,
                Default::default(),
            )
            .await?;
        show_binding(app, &active_worker, request.response()?.shortcuts());
        loop {
            tokio::select! {
                event = activated.next() => match event {
                    Some(event) if Some(event.session_handle().as_str()) == path.as_str() && event.shortcut_id() == ID => {
                        let token = event.options().get("activation_token")
                            .and_then(|value| <&str>::try_from(value).ok())
                            .filter(|token| !token.is_empty() && token.len() <= 4096 && !token.contains('\0'));
                        if !*active_worker.borrow() && enabled(app) {
                            editor_lifecycle::reveal_with_activation(app, token.map(str::to_owned));
                        }
                    }
                    Some(_) => {}
                    None => break,
                },
                event = changed.next() => match event {
                    Some(event) if Some(event.session_handle().as_str()) == path.as_str() => show_binding(app, &active_worker, event.shortcuts()),
                    Some(_) => {}
                    None => break,
                },
                _ = closed.next() => break,
            }
        }
        Ok(())
    };
    let result = tokio::select! {
        biased;
        _ = cancelled.changed() => Ok(()),
        result = operation => result,
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), session.close()).await;
    result
}

// Run for the current operation
pub(super) async fn run(app: AppHandle, mut cancelled: tokio::sync::watch::Receiver<bool>) {
    let monitor = async {
        let monitor = zbus::Connection::session().await.ok()?;
        let dbus = DBusProxy::new(&monitor).await.ok()?;
        let mut owners = dbus
            .receive_name_owner_changed_with_args(&[(0, PORTAL)])
            .await
            .ok()?;
        while !*cancelled.borrow() {
            report(&app, &cancelled, "Waiting for the desktop shortcut portal…");
            let mut session_cancel = cancelled.clone();
            let active = async {
                let connection = zbus::Connection::session().await?;
                bind(&app, connection, &mut session_cancel).await
            };
            let result = tokio::select! {
                biased;
                result = active => result,
                owner = owners.next() => { owner?; continue; }
            };
            if *cancelled.borrow() {
                return Some(());
            }
            let message = if matches!(result, Err(ashpd::Error::Response(_))) {
                "The desktop shortcut was declined. Toggle it again to retry or bind stream-dock-manager --show."
            } else {
                UNAVAILABLE
            };
            report(&app, &cancelled, message);
            crate::manager_log::warning("Wayland shortcut was declined, closed, or unavailable");
            tokio::select! {
                _ = cancelled.changed() => return Some(()),
                owner = owners.next() => { owner?; }
            }
        }
        Some(())
    };
    if monitor.await.is_none() && !*cancelled.borrow() {
        report(&app, &cancelled, UNAVAILABLE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify only a missing registry allows legacy identity inference
    fn only_a_missing_registry_allows_legacy_identity_inference() {
        assert!(registry_unavailable(&ashpd::Error::PortalNotFound(
            REGISTRY.try_into().unwrap()
        )));
        assert!(registry_unavailable(&ashpd::Error::Zbus(
            zbus::fdo::Error::UnknownMethod("Register".into()).into()
        )));
        assert!(!registry_unavailable(&ashpd::Error::Zbus(
            zbus::fdo::Error::AccessDenied("App ID rejected".into()).into()
        )));
    }
}
