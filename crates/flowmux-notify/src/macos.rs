// SPDX-License-Identifier: GPL-3.0-or-later
//! Native Notification Center delivery and main-thread Dock updates.
use block2::{DynBlock, RcBlock};
use flowmux_core::Notification;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker};
use objc2_app_kit::NSApplication;
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSString,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::oneshot;

pub const OPEN_NOTIFICATION_ACTION: &str = "open-notification";
pub const DESKTOP_FILE_BASENAME: &str = "com.flowmux.App";

#[derive(Clone)]
pub struct DesktopNotifier;

impl DesktopNotifier {
    pub async fn connect() -> zbus::Result<Self> {
        Ok(Self)
    }

    pub async fn send(&self, notification: &Notification) -> zbus::Result<String> {
        receive(authorize()?).await?;
        let id = uuid::Uuid::new_v4().to_string();
        receive(submit(notification, &id)?).await?;
        Ok(id)
    }

    pub async fn close(&self, desktop_id: &str) -> zbus::Result<()> {
        let center = center()?;
        let identifiers = NSArray::from_slice(&[&*NSString::from_str(desktop_id)]);
        center.removePendingNotificationRequestsWithIdentifiers(&identifiers);
        center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
        Ok(())
    }

    pub async fn update_launcher_count(&self, _app_uri: &str, count: i64) -> zbus::Result<()> {
        let (done, result) = oneshot::channel();
        dispatch2::DispatchQueue::main().exec_async(move || {
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
            app.dockTile().setBadgeLabel(label.as_deref());
            let _ = done.send(Ok(()));
        });
        receive(result).await
    }
}

fn center() -> zbus::Result<Retained<UNUserNotificationCenter>> {
    // UserNotifications throws an Objective-C exception for an unbundled executable.
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        return Err(zbus::Error::Failure(
            "Notification Center requires launching the Flowmux.app bundle".into(),
        ));
    }
    Ok(UNUserNotificationCenter::currentNotificationCenter())
}

type Completion = oneshot::Receiver<Result<(), String>>;

async fn receive(result: Completion) -> zbus::Result<()> {
    result
        .await
        .map_err(|error| zbus::Error::Failure(error.to_string()))?
        .map_err(zbus::Error::Failure)
}

fn authorization_result(granted: bool, error: Option<String>) -> Result<(), String> {
    match (granted, error) {
        (_, Some(error)) => Err(error),
        (true, None) => Ok(()),
        (false, None) => Err("Notifications are disabled for Flowmux in System Settings".into()),
    }
}

fn authorize() -> zbus::Result<Completion> {
    let center = center()?;
    let (done, result) = oneshot::channel();
    let done = Mutex::new(Some(done));
    let callback = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        let error = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
        if let Some(done) = done.lock().unwrap().take() {
            let _ = done.send(authorization_result(granted.as_bool(), error));
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert
            | UNAuthorizationOptions::Sound
            | UNAuthorizationOptions::Badge,
        &callback,
    );
    Ok(result)
}

fn request(notification: &Notification, id: &str) -> Retained<UNNotificationRequest> {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&notification.title));
    content.setBody(&NSString::from_str(&notification.body));
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    let target = NSString::from_str(&format!("{}:{}", std::process::id(), notification.id));
    let info = NSDictionary::from_slices(&[&*NSString::from_str("target")], &[&*target]);
    unsafe { content.setUserInfo(info.cast_unchecked()) };
    UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(id),
        &content,
        None,
    )
}

fn submit(notification: &Notification, id: &str) -> zbus::Result<Completion> {
    let center = center()?;
    let (done, result) = oneshot::channel();
    let done = Mutex::new(Some(done));
    let callback = RcBlock::new(move |error: *mut NSError| {
        let error = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
        if let Some(done) = done.lock().unwrap().take() {
            let _ = done.send(error.map_or(Ok(()), Err));
        }
    });
    center
        .addNotificationRequest_withCompletionHandler(&request(notification, id), Some(&callback));
    Ok(result)
}

type OpenNotification = Arc<dyn Fn(String) + Send + Sync>;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "FlowmuxNotificationDelegate"]
    #[ivars = OpenNotification]
    struct NotificationDelegate;

    unsafe impl NSObjectProtocol for NotificationDelegate {}
    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            done: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            done.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn respond(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            done: &DynBlock<dyn Fn()>,
        ) {
            if &*response.actionIdentifier() == unsafe { UNNotificationDefaultActionIdentifier } {
                let info = response.notification().request().content().userInfo();
                if let Some(value) = info.objectForKey(NSString::from_str("target").as_ref()) {
                    if let Some(target) = value.downcast_ref::<NSString>() {
                        (self.ivars())(target.to_string());
                    }
                }
            }
            done.call(());
        }
    }
);

// The delegate holds only an immutable Send + Sync callback; Cocoa invokes it on a background queue.
unsafe impl Send for NotificationDelegate {}
unsafe impl Sync for NotificationDelegate {}

/// Retain the weak Notification Center delegate for the process lifetime.
pub fn register_action_router(open: impl Fn(String) + Send + Sync + 'static) -> zbus::Result<()> {
    static DELEGATE: OnceLock<Retained<NotificationDelegate>> = OnceLock::new();
    let center = center()?;
    let delegate = DELEGATE.get_or_init(|| unsafe {
        msg_send![
            super(NotificationDelegate::alloc().set_ivars(Arc::new(open))),
            init
        ]
    });
    center.setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_or_failed_authorization_is_not_delivery_success() {
        assert!(authorization_result(false, None).is_err());
        assert_eq!(
            authorization_result(true, Some("error".into())),
            Err("error".into())
        );
        assert_eq!(authorization_result(true, None), Ok(()));
    }

    #[test]
    fn request_preserves_content_and_routes_to_originating_process() {
        let notification = Notification {
            id: flowmux_core::NotificationId::new(),
            level: flowmux_core::NotificationLevel::NeedsInput,
            title: "에이전트".into(),
            body: "입력이 필요합니다".into(),
            source_pane: None,
            created_at: chrono::Utc::now(),
            read: false,
        };
        let request = request(&notification, "desktop-id");
        assert_eq!(request.identifier().to_string(), "desktop-id");
        assert_eq!(request.content().title().to_string(), notification.title);
        assert_eq!(request.content().body().to_string(), notification.body);
        let target = request
            .content()
            .userInfo()
            .objectForKey(NSString::from_str("target").as_ref())
            .unwrap();
        assert_eq!(
            target.downcast_ref::<NSString>().unwrap().to_string(),
            format!("{}:{}", std::process::id(), notification.id)
        );
        assert!(request.trigger().is_none());
        assert!(
            request.content().badge().is_none(),
            "unread count owns the Dock badge"
        );
    }
}
