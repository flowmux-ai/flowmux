// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_core::{Notification, NotificationId, NotificationLevel};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

struct DesktopFixture(Arc<Mutex<HashSet<String>>>);

#[zbus::interface(name = "org.gtk.Notifications")]
impl DesktopFixture {
    async fn add_notification(
        &self,
        app_id: &str,
        id: &str,
        notification: HashMap<String, zbus::zvariant::OwnedValue>,
    ) {
        assert_eq!(app_id, flowmux_notify::DESKTOP_FILE_BASENAME);
        assert!(notification.contains_key("title"));
        self.0.lock().unwrap().insert(id.into());
    }

    async fn remove_notification(&self, app_id: &str, id: &str) {
        assert_eq!(app_id, flowmux_notify::DESKTOP_FILE_BASENAME);
        self.0.lock().unwrap().remove(id);
    }
}

async fn push(controller: &WindowController) -> NotificationId {
    let (ack, reply) = oneshot::channel();
    controller
        .dispatch(GtkCommand::AddNotification {
            pane: None,
            surface: None,
            workspace: None,
            title: "Cleanup fixture".into(),
            body: "native notification cleanup".into(),
            level: NotificationLevel::Info,
            ack,
        })
        .await;
    reply.await.unwrap().unwrap()
}

async fn desktop(notifier: &flowmux_notify::DesktopNotifier, id: NotificationId) -> String {
    notifier
        .send(&Notification {
            id,
            level: NotificationLevel::Info,
            title: "Cleanup fixture".into(),
            body: "".into(),
            source_pane: None,
            created_at: chrono::Utc::now(),
            read: false,
        })
        .await
        .unwrap()
}

pub(super) async fn check(app: &adw::Application) {
    let open = Arc::new(Mutex::new(HashSet::new()));
    let _service = zbus::connection::Builder::session()
        .unwrap()
        .name("org.gtk.Notifications")
        .unwrap()
        .serve_at("/org/gtk/Notifications", DesktopFixture(open.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();
    let notifier = flowmux_notify::DesktopNotifier::connect().await.unwrap();
    let (bridge, _rx) = Bridge::new();
    let controller = WindowController::new(
        app,
        StateStore::new_lazy_ephemeral(State::default()),
        Arc::new(ResolvedTheme::load()),
        bridge,
        gtk::CssProvider::new(),
        Some(tokio::runtime::Handle::current()),
    );
    controller.window.present();
    wait_until("notification window mapped", || {
        controller.window.is_mapped()
    })
    .await;

    let id = push(&controller).await;
    let desktop_id = desktop(&notifier, id).await;
    controller.dispatch(GtkCommand::ClearAllNotifications).await;
    controller
        .dispatch(GtkCommand::SetNotificationDesktopId { id, desktop_id })
        .await;
    wait_until("late cleared notification withdrawn", || {
        open.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(controller.notifications.unread_count(), 0);

    let id = push(&controller).await;
    let desktop_id = desktop(&notifier, id).await;
    controller
        .dispatch(GtkCommand::SetNotificationDesktopId { id, desktop_id })
        .await;
    for _ in 0..50 {
        push(&controller).await;
    }
    assert!(controller.notifications.find(id).is_none());
    assert_eq!(controller.notifications.entries().len(), 50);
    wait_until("evicted notification withdrawn", || {
        open.lock().unwrap().is_empty()
    })
    .await;

    let id = push(&controller).await;
    let desktop_id = desktop(&notifier, id).await;
    controller
        .dispatch(GtkCommand::DeleteNotification { id })
        .await;
    controller
        .dispatch(GtkCommand::SetNotificationDesktopId { id, desktop_id })
        .await;
    wait_until("late deleted notification withdrawn", || {
        open.lock().unwrap().is_empty()
    })
    .await;
    controller.window.destroy();
    println!("MACOS_NOTIFICATION_CLEANUP_OK");
}
