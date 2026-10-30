// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_core::{NotificationId, NotificationLevel};

fn badge() -> Option<String> {
    objc2_app_kit::NSApplication::sharedApplication(objc2::MainThreadMarker::new().unwrap())
        .dockTile()
        .badgeLabel()
        .map(|label| label.to_string())
}

async fn push(controller: &WindowController) -> NotificationId {
    let (ack, reply) = oneshot::channel();
    controller
        .dispatch(GtkCommand::AddNotification {
            id: None,
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

pub(super) async fn check(app: &adw::Application) {
    let (bridge, _rx) = Bridge::new();
    let mut controller = WindowController::new(
        app,
        StateStore::new_lazy_ephemeral(State::default()),
        Arc::new(ResolvedTheme::load()),
        bridge,
        gtk::CssProvider::new(),
        Some(tokio::runtime::Handle::current()),
    );
    let notifier = Arc::new(tokio::sync::Mutex::new(Some(
        flowmux_notify::DesktopNotifier::connect().await.unwrap(),
    )));
    controller
        .notifications
        .use_shared_notifier(notifier.clone());
    // Desktop delivery holds this lock while macOS waits for notification permission.
    let pending_permission = notifier.lock().await;
    controller.window.present();
    wait_until("notification window mapped", || {
        controller.window.is_mapped()
    })
    .await;

    let id = push(&controller).await;
    let desktop_id = format!("cleanup-{id}");
    wait_until("unread Dock badge", || badge().as_deref() == Some("1")).await;
    controller.dispatch(GtkCommand::ClearAllNotifications).await;
    controller
        .dispatch(GtkCommand::SetNotificationDesktopId { id, desktop_id })
        .await;
    wait_until("cleared Dock badge", || badge().is_none()).await;
    assert_eq!(controller.notifications.unread_count(), 0);
    drop(pending_permission);

    let id = push(&controller).await;
    let desktop_id = format!("cleanup-{id}");
    controller
        .dispatch(GtkCommand::SetNotificationDesktopId { id, desktop_id })
        .await;
    for _ in 0..50 {
        push(&controller).await;
    }
    assert!(controller.notifications.find(id).is_none());
    assert_eq!(controller.notifications.entries().len(), 50);
    wait_until("bounded unread Dock badge", || {
        badge().as_deref() == Some("50")
    })
    .await;

    let id = push(&controller).await;
    let desktop_id = format!("cleanup-{id}");
    controller
        .dispatch(GtkCommand::DeleteNotification { id })
        .await;
    controller
        .dispatch(GtkCommand::SetNotificationDesktopId { id, desktop_id })
        .await;
    wait_until("deleted Dock badge", || badge().as_deref() == Some("49")).await;
    assert!(controller.notifications.find(id).is_none());
    controller.dispatch(GtkCommand::ClearAllNotifications).await;
    wait_until("final Dock badge cleared", || badge().is_none()).await;
    controller.window.destroy();
    println!("MACOS_NOTIFICATION_CLEANUP_OK");
}
