// SPDX-License-Identifier: GPL-3.0-or-later
//! Send desktop notifications through `org.gtk.Notifications`, using a UUID
//! per notification for later withdrawal. This requires a session service
//! implementing that interface; this sender has no freedesktop fallback.
//!
//! Launcher badge counts are published separately through Unity LauncherEntry.

use flowmux_core::{Notification, NotificationLevel};
use std::collections::HashMap;
use zbus::{proxy, zvariant::Value, Connection};

/// Application action invoked when the user clicks a desktop notification.
/// The action target is `<flowmux-pid>:<notification-id>` so the process that
/// owns the well-known app D-Bus name can forward the click to the window that
/// emitted it without launching another flowmux process.
pub const OPEN_NOTIFICATION_ACTION: &str = "open-notification";
const OPEN_NOTIFICATION_DETAILED_ACTION: &str = "app.open-notification";

/// Object path for Unity LauncherEntry badge updates. Notification withdrawal
/// and launcher count updates are separate D-Bus operations.
const LAUNCHER_ENTRY_PATH: &str = "/com/canonical/unity/launcherentry/flowmux";
const LAUNCHER_ENTRY_INTERFACE: &str = "com.canonical.Unity.LauncherEntry";
const LAUNCHER_ENTRY_MEMBER: &str = "Update";

/// Basename of the installed desktop file (`com.flowmux.App.desktop`)
/// without the `.desktop` extension. Used as the `app_id` argument on
/// every `AddNotification` / `RemoveNotification` call so GNOME Shell
/// binds the notification to flowmux's launcher icon. A drift between
/// this string and the real desktop file name lands the dock badge on
/// a non-existent app id and the user is left with a stuck counter.
///
/// Keep this in lockstep with `crates/flowmux/src/main.rs::APP_ID`
/// and `resources/desktop/com.flowmux.App.desktop`.
pub const DESKTOP_FILE_BASENAME: &str = "com.flowmux.App";

/// Proxy for a session service implementing `org.gtk.Notifications`.
#[proxy(
    interface = "org.gtk.Notifications",
    default_service = "org.gtk.Notifications",
    default_path = "/org/gtk/Notifications"
)]
trait GtkNotifications {
    fn add_notification(
        &self,
        app_id: &str,
        id: &str,
        notification: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<()>;

    fn remove_notification(&self, app_id: &str, id: &str) -> zbus::Result<()>;
}

#[derive(Clone)]
pub struct DesktopNotifier {
    conn: Connection,
}

impl DesktopNotifier {
    pub async fn connect() -> zbus::Result<Self> {
        Ok(Self {
            conn: Connection::session().await?,
        })
    }

    /// Send a notification and return the UUID required by [`Self::close`].
    /// Launcher badge counts require a separate [`Self::update_launcher_count`] call.
    pub async fn send(&self, n: &Notification) -> zbus::Result<String> {
        let proxy = GtkNotificationsProxy::new(&self.conn).await?;
        let id = uuid::Uuid::new_v4().to_string();
        let action_target = format!("{}:{}", std::process::id(), n.id);
        let notif = notification_payload(n, &action_target);
        // Omit the serialized GIcon so the service can use the app's launcher icon.
        proxy
            .add_notification(DESKTOP_FILE_BASENAME, &id, notif)
            .await?;
        Ok(id)
    }

    /// Withdraw a previously sent notification. On GNOME this destroys
    /// the `MessageTray.Source` entry and removes the message-tray dot
    /// next to the launcher icon. The *number circle* on Ubuntu Dock is
    /// driven by [`Self::update_launcher_count`] instead — call both
    /// when you want the entire dock indicator to converge with the
    /// in-app unread count. Idempotent: an unknown id is a benign
    /// no-op on the server side.
    pub async fn close(&self, desktop_id: &str) -> zbus::Result<()> {
        let proxy = GtkNotificationsProxy::new(&self.conn).await?;
        proxy
            .remove_notification(DESKTOP_FILE_BASENAME, desktop_id)
            .await
    }

    /// Publish the unread-notification count to the dock badge via the
    /// `com.canonical.Unity.LauncherEntry::Update` D-Bus signal. Ubuntu
    /// Dock, Dash-to-Dock, KDE Plasma and plank all listen for this
    /// signal to drive their per-app number circle. `count <= 0` hides
    /// the badge by sending `count-visible = false`.
    ///
    /// `app_uri` should be `application://<desktop-file-name>.desktop`
    /// (e.g. `application://com.flowmux.App.desktop`) so the dock can
    /// associate the badge with our launcher icon.
    pub async fn update_launcher_count(&self, app_uri: &str, count: i64) -> zbus::Result<()> {
        let visible = count > 0;
        let mut props: HashMap<&str, Value<'_>> = HashMap::new();
        props.insert("count", Value::I64(count.max(0)));
        props.insert("count-visible", Value::Bool(visible));
        // `urgent = true` makes some docks bounce / glow the icon. We
        // mirror visibility so the icon goes back to neutral once the
        // count hits zero.
        props.insert("urgent", Value::Bool(visible));
        self.conn
            .emit_signal(
                None::<&str>,
                LAUNCHER_ENTRY_PATH,
                LAUNCHER_ENTRY_INTERFACE,
                LAUNCHER_ENTRY_MEMBER,
                &(app_uri, props),
            )
            .await
    }
}

fn notification_payload<'a>(
    n: &'a Notification,
    action_target: &'a str,
) -> HashMap<&'static str, Value<'a>> {
    let mut notif = HashMap::new();
    notif.insert("title", Value::Str(n.title.as_str().into()));
    notif.insert("body", Value::Str(n.body.as_str().into()));
    notif.insert("priority", Value::Str(priority_for(n.level).into()));
    notif.insert(
        "default-action",
        Value::Str(OPEN_NOTIFICATION_DETAILED_ACTION.into()),
    );
    notif.insert("default-action-target", Value::Str(action_target.into()));
    notif
}

fn priority_for(level: NotificationLevel) -> &'static str {
    match level {
        NotificationLevel::Info | NotificationLevel::TurnCompleted => "normal",
        NotificationLevel::NeedsInput => "high",
        NotificationLevel::Error => "urgent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app_id we pass to `AddNotification` must match the installed
    /// `.desktop` basename; otherwise GNOME Shell binds the notification
    /// to a non-existent launcher and Ubuntu Dock's per-app counter
    /// never finds it, leaving the badge stuck after we ack.
    #[test]
    fn desktop_file_basename_matches_installed_desktop_file() {
        assert_eq!(
            DESKTOP_FILE_BASENAME, "com.flowmux.App",
            "DESKTOP_FILE_BASENAME must match resources/desktop/<basename>.desktop \
             and the GApplication application_id; otherwise the dock badge survives ack",
        );
    }

    /// Keep completion informational and input/error notifications elevated.
    #[test]
    fn priority_for_levels_maps_to_gtk_notifications_strings() {
        assert_eq!(priority_for(NotificationLevel::Info), "normal");
        assert_eq!(priority_for(NotificationLevel::TurnCompleted), "normal");
        assert_eq!(priority_for(NotificationLevel::NeedsInput), "high");
        assert_eq!(priority_for(NotificationLevel::Error), "urgent");
    }

    #[test]
    fn desktop_notification_click_targets_the_originating_entry() {
        let notification = Notification {
            id: flowmux_core::NotificationId::new(),
            level: NotificationLevel::NeedsInput,
            title: "Codex".into(),
            body: "Needs input".into(),
            source_pane: None,
            created_at: chrono::Utc::now(),
            read: false,
        };
        let target = format!("1234:{}", notification.id);
        let payload = notification_payload(&notification, &target);

        assert_eq!(
            <&str>::try_from(payload.get("default-action").unwrap()).unwrap(),
            OPEN_NOTIFICATION_DETAILED_ACTION
        );
        assert_eq!(
            <&str>::try_from(payload.get("default-action-target").unwrap()).unwrap(),
            target
        );
        assert_eq!(
            OPEN_NOTIFICATION_DETAILED_ACTION,
            format!("app.{OPEN_NOTIFICATION_ACTION}")
        );
    }
}
