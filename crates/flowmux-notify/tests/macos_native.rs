// SPDX-License-Identifier: GPL-3.0-or-later
//! Main-thread Dock checks run in CI without notification permission.
//! For OS delivery, run this executable in a signed, disposable app under Applications
//! with FLOWMUX_NOTIFICATION_DELIVERY_SMOKE=1; use `click` to also wait for a real click.
#[cfg(target_os = "macos")]
fn main() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::{NSDate, NSRunLoop};
    use std::time::{Duration, Instant};

    let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delivery = std::env::var_os("FLOWMUX_NOTIFICATION_DELIVERY_SMOKE").is_some();
    let (clicked, click) = std::sync::mpsc::channel();
    if delivery {
        flowmux_notify::sender::register_action_router(move |target| {
            clicked.send(target).unwrap();
        })
        .unwrap();
    }
    app.finishLaunching();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for count in [7, 0, -1] {
        let job = runtime.spawn(async move {
            let notifier = flowmux_notify::DesktopNotifier::connect().await.unwrap();
            notifier
                .update_launcher_count("application://com.flowmux.NotificationSmoke.desktop", count)
                .await
                .unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !job.is_finished() {
            assert!(Instant::now() < deadline, "native badge update timed out");
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.01));
        }
        runtime.block_on(job).unwrap();
        let label = app.dockTile().badgeLabel().map(|label| label.to_string());
        assert_eq!(label.as_deref(), (count > 0).then_some("7"));
    }
    println!("MACOS_NATIVE_DOCK_BADGE_OK");
    if delivery {
        native_delivery(&runtime, click);
    } else {
        let error = runtime.block_on(async {
            flowmux_notify::DesktopNotifier::connect()
                .await
                .unwrap()
                .send(&notification())
                .await
                .unwrap_err()
        });
        assert!(error.to_string().contains("Flowmux.app bundle"));
        println!("MACOS_UNBUNDLED_NOTIFICATION_ERROR_OK");
    }
}

#[cfg(target_os = "macos")]
fn notification() -> flowmux_core::Notification {
    flowmux_core::Notification {
        id: flowmux_core::NotificationId::new(),
        level: flowmux_core::NotificationLevel::NeedsInput,
        title: "Flowmux notification test".into(),
        body: "격리된 테스트입니다. 이 알림을 클릭해 주세요.".into(),
        source_pane: None,
        created_at: chrono::Utc::now(),
        read: false,
    }
}

#[cfg(target_os = "macos")]
fn pump_until(mut ready: impl FnMut() -> bool) {
    use objc2_foundation::{NSDate, NSRunLoop};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "native notification operation timed out"
        );
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.01));
    }
}

#[cfg(target_os = "macos")]
fn delivered_ids() -> Vec<String> {
    use objc2_foundation::NSArray;
    use objc2_user_notifications::{UNNotification, UNUserNotificationCenter};
    let (done, result) = std::sync::mpsc::channel();
    let callback = block2::RcBlock::new(
        move |notifications: std::ptr::NonNull<NSArray<UNNotification>>| {
            let notifications = unsafe { notifications.as_ref() };
            let ids = (0..notifications.count())
                .map(|index| {
                    notifications
                        .objectAtIndex(index)
                        .request()
                        .identifier()
                        .to_string()
                })
                .collect();
            done.send(ids).unwrap();
        },
    );
    UNUserNotificationCenter::currentNotificationCenter()
        .getDeliveredNotificationsWithCompletionHandler(&callback);
    let mut ids = None;
    pump_until(|| {
        ids = result.try_recv().ok();
        ids.is_some()
    });
    ids.unwrap()
}

#[cfg(target_os = "macos")]
fn native_delivery(runtime: &tokio::runtime::Runtime, click: std::sync::mpsc::Receiver<String>) {
    objc2_user_notifications::UNUserNotificationCenter::currentNotificationCenter()
        .removeAllDeliveredNotifications();
    for immediate in [false, true] {
        let job = runtime.spawn(async move {
            let notifier = flowmux_notify::DesktopNotifier::connect().await.unwrap();
            let id = notifier.send(&notification()).await.unwrap();
            if immediate {
                notifier.close(&id).await.unwrap();
            }
            id
        });
        pump_until(|| job.is_finished());
        let id = runtime.block_on(job).unwrap();
        if !immediate {
            pump_until(|| delivered_ids().contains(&id));
            println!("MACOS_NOTIFICATION_DELIVERED id={id}");
            runtime.block_on(async {
                flowmux_notify::DesktopNotifier::connect()
                    .await
                    .unwrap()
                    .close(&id)
                    .await
                    .unwrap();
            });
            pump_until(|| !delivered_ids().contains(&id));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        pump_until(|| {
            assert!(
                !delivered_ids().contains(&id),
                "withdrawn notification reappeared"
            );
            std::time::Instant::now() >= deadline
        });
    }
    println!("MACOS_NOTIFICATION_WITHDRAWAL_OK");
    if std::env::var("FLOWMUX_NOTIFICATION_DELIVERY_SMOKE").as_deref() != Ok("click") {
        return;
    }
    let sample = notification();
    let target = format!("{}:{}", std::process::id(), sample.id);
    let job = runtime.spawn(async move {
        flowmux_notify::DesktopNotifier::connect()
            .await
            .unwrap()
            .send(&sample)
            .await
    });
    pump_until(|| job.is_finished());
    let id = runtime.block_on(job).unwrap().unwrap();
    pump_until(|| delivered_ids().contains(&id));
    println!("MACOS_NOTIFICATION_AWAITING_CLICK id={id}");
    let mut clicked = None;
    pump_until(|| {
        clicked = click.try_recv().ok();
        clicked.is_some()
    });
    assert_eq!(clicked.as_deref(), Some(target.as_str()));
    println!("MACOS_NOTIFICATION_CLICK_OK");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
