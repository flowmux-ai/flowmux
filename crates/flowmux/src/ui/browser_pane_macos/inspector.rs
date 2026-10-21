// SPDX-License-Identifier: GPL-3.0-or-later
//! WebKit exposes inspectability publicly, but opening its local UI uses SPI.
//! Keep those selectors here and check availability before sending messages.
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{msg_send, sel};
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
    let inspector = inspector(view).filter(|object| responds(object, sel!(show)))
        .ok_or("This macOS version cannot open Web Inspector here. Select this Flowmux page in Safari's Develop menu.")?;
    unsafe {
        let _: () = msg_send![&*inspector, show];
    }
    Ok(())
}

pub(super) fn close(view: &WKWebView) {
    if let Some(inspector) = inspector(view).filter(|object| responds(object, sel!(close))) {
        unsafe {
            let _: () = msg_send![&*inspector, close];
        }
    }
}
