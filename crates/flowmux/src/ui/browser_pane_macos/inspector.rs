// SPDX-License-Identifier: GPL-3.0-or-later
//! WebKit exposes inspectability publicly, but opening its local UI uses SPI.
//! Keep those selectors here and check availability before sending messages.
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{msg_send, sel, ClassType};
use objc2_web_kit::{WKPreferences, WKWebView};

fn responds(object: &AnyObject, selector: Sel) -> bool {
    unsafe {
        let supported: Bool = msg_send![object, respondsToSelector: selector];
        supported.as_bool()
    }
}

pub(super) fn configure(preferences: &WKPreferences) {
    if responds(preferences, sel!(_setDeveloperExtrasEnabled:)) {
        unsafe {
            let _: () = msg_send![preferences, _setDeveloperExtrasEnabled: Bool::YES];
        }
    }
}

fn inspector(view: &WKWebView) -> Option<Retained<AnyObject>> {
    if !responds(view, sel!(_inspector)) {
        return None;
    }
    unsafe { msg_send![view, _inspector] }
}

pub(super) fn show(view: &WKWebView) -> Result<(), &'static str> {
    let inspector = inspector(view).filter(|object| responds(object, sel!(show)) && responds(object, sel!(detach)))
        .ok_or("This macOS version cannot open Web Inspector here. Select this Flowmux page in Safari's Develop menu.")?;
    unsafe {
        let _: () = msg_send![&*inspector, show];
    }
    detach(&inspector, view);
    Ok(())
}

// Also called when the asynchronous frontend connects, before WebKit docks it.
pub(super) fn detach(inspector: &AnyObject, view: &WKWebView) {
    if responds(inspector, sel!(detach)) {
        let frame = view.as_super().frame();
        unsafe {
            let _: () = msg_send![inspector, detach];
        }
        // WebKit otherwise expands the page to the whole GTK content view.
        view.as_super().setFrame(frame);
    }
}

pub(super) fn detach_if_visible(view: &WKWebView) {
    if let Some(inspector) = inspector(view).filter(|object| responds(object, sel!(isVisible))) {
        let visible: Bool = unsafe { msg_send![&*inspector, isVisible] };
        if visible.as_bool() {
            detach(&inspector, view);
        }
    }
}

pub(super) fn close(view: &WKWebView) {
    if let Some(inspector) = inspector(view).filter(|object| responds(object, sel!(close))) {
        unsafe {
            let _: () = msg_send![&*inspector, close];
        }
    }
}
