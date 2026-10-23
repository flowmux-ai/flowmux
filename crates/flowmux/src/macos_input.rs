// SPDX-License-Identifier: GPL-3.0-or-later
//! Native editing confined to a process-owned WKWebView.
use objc2::runtime::Bool;
use objc2::{msg_send, sel, ClassType};
use objc2_app_kit::NSView;
use objc2_foundation::{NSNotFound, NSRange, NSString};
use objc2_web_kit::WKWebView;

pub(crate) fn insert_text(web: &WKWebView, text: &str) -> Result<(), String> {
    let responder = web
        .as_super()
        .window()
        .ok_or("browser view is not attached")?
        .firstResponder()
        .filter(|responder| {
            responder
                .downcast_ref::<NSView>()
                .is_some_and(|view| view.isDescendantOf(web.as_super()))
        })
        .ok_or("browser does not own keyboard focus")?;
    let supported: Bool =
        unsafe { msg_send![&*responder, respondsToSelector: sel!(insertText:replacementRange:)] };
    if !supported.as_bool() {
        return Err("focused view cannot insert text".into());
    }
    unsafe {
        let _: () = msg_send![&*responder, insertText: &*NSString::from_str(text), replacementRange: NSRange::new(NSNotFound as usize, 0)];
    }
    Ok(())
}
