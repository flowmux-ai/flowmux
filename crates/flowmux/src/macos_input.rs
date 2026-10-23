// SPDX-License-Identifier: GPL-3.0-or-later
//! Native editing confined to a process-owned WKWebView.
use objc2::runtime::Bool;
use objc2::{msg_send, sel, ClassType};
use objc2_app_kit::{NSEvent, NSEventModifierFlags as Flags, NSEventType, NSView};
use objc2_foundation::{NSNotFound, NSPoint, NSRange, NSString};
use objc2_web_kit::WKWebView;

pub(crate) fn physical_key(key: &str) -> Option<(&str, u16)> {
    Some(match key {
        "Enter" => ("\r", 36),
        "Tab" => ("\t", 48),
        "Escape" => ("\u{1b}", 53),
        "Backspace" => ("\u{7f}", 51),
        "Delete" => ("\u{f728}", 117),
        "Insert" => ("\u{f727}", 114),
        "Home" => ("\u{f729}", 115),
        "End" => ("\u{f72b}", 119),
        "PageUp" => ("\u{f72c}", 116),
        "PageDown" => ("\u{f72d}", 121),
        "ArrowLeft" => ("\u{f702}", 123),
        "ArrowRight" => ("\u{f703}", 124),
        "ArrowUp" => ("\u{f700}", 126),
        "ArrowDown" => ("\u{f701}", 125),
        "F1" => ("\u{f704}", 122),
        "F2" => ("\u{f705}", 120),
        "F3" => ("\u{f706}", 99),
        "F4" => ("\u{f707}", 118),
        "F5" => ("\u{f708}", 96),
        "F6" => ("\u{f709}", 97),
        "F7" => ("\u{f70a}", 98),
        "F8" => ("\u{f70b}", 100),
        "F9" => ("\u{f70c}", 101),
        "F10" => ("\u{f70d}", 109),
        "F11" => ("\u{f70e}", 103),
        "F12" => ("\u{f70f}", 111),
        _ if key.is_ascii() && key.len() == 1 => {
            let shifted = "~!@#$%^&*()_+{}|:\"<>?";
            let plain = "`1234567890-=[]\\;',./";
            let base = shifted
                .find(key)
                .map(|i| &plain[i..i + 1])
                .unwrap_or(key)
                .to_ascii_lowercase();
            let codes = "asdfhgzxcv\0bqweryt123465=97-80]ou[ip\rlj'k;\\,/nm.\t `";
            (key, codes.find(&base)? as u16)
        }
        _ => return None,
    })
}

pub(crate) fn press_key(
    web: &WKWebView,
    stroke: &flowmux_browser::input::KeyStroke,
) -> Result<(), String> {
    let (text, code) =
        physical_key(&stroke.key).ok_or("key is not available; use type for text")?;
    let window = web
        .as_super()
        .window()
        .ok_or("browser view is not attached")?;
    if !window.firstResponder().is_some_and(|responder| {
        responder
            .downcast_ref::<NSView>()
            .is_some_and(|view| view.isDescendantOf(web.as_super()))
    }) {
        return Err("browser does not own keyboard focus".into());
    }
    let mut flags = Flags::empty();
    if stroke.control {
        flags |= Flags::Control;
    }
    if stroke.alt {
        flags |= Flags::Option;
    }
    if stroke.meta {
        flags |= Flags::Command;
    }
    let shifted = "~!@#$%^&*()_+{}|:\"<>?";
    if stroke.shift
        || (text.len() == 1
            && (text.chars().all(|c| c.is_ascii_uppercase()) || shifted.contains(text)))
    {
        flags |= Flags::Shift;
    }
    let characters = if stroke.key.len() == 1 {
        stroke.event_key()
    } else {
        text.to_string()
    };
    let chars = NSString::from_str(&characters);
    for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind, NSPoint::new(0.0,0.0), flags, gtk::glib::monotonic_time() as f64 / 1_000_000.0,
            window.windowNumber(), None, &chars, &NSString::from_str(text), false, code,
        ).ok_or("could not create keyboard event")?;
        if kind == NSEventType::KeyUp
            || !flags.contains(Flags::Command)
            || !web.performKeyEquivalent(&event)
        {
            window.sendEvent(&event);
        }
    }
    Ok(())
}

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

pub(crate) fn pointer(
    web: &WKWebView,
    action: flowmux_browser::input::PointerAction,
    mut point: NSPoint,
) -> Result<(), String> {
    use flowmux_browser::input::{PointerAction, PointerTarget};
    let view = web.as_super();
    let bounds = view.bounds();
    PointerTarget {
        x: point.x,
        y: point.y,
        width: bounds.size.width,
        height: bounds.size.height,
    }
    .scale(bounds.size.width, bounds.size.height)?;
    let window = view.window().ok_or("browser view is not attached")?;
    if !view.isFlipped() {
        point.y = bounds.size.height - point.y;
    }
    let content = window
        .contentView()
        .ok_or("browser has no native content view")?;
    let hit = content.hitTest(view.convertPoint_toView(point, Some(&content)));
    if !hit.as_ref().is_some_and(|hit| hit.isDescendantOf(view)) {
        tracing::debug!(target = ?point, bounds = ?bounds, web_frame = ?view.frame(),
            content_frame = ?content.frame(), content_flipped = content.isFlipped(),
            hit = ?hit, "native browser pointer target is covered");
        return Err("browser pointer target is covered by a native view".into());
    }
    let point = view.convertPoint_toView(point, None);
    let event = |kind, count| {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind, point, Flags::empty(), gtk::glib::monotonic_time() as f64 / 1_000_000.0,
            window.windowNumber(), None, 0, count, 1.0,
        ).ok_or("could not create pointer event")
    };
    // WKWebView routes motion through its tracking owner, not NSResponder::mouseMoved.
    let motion = event(NSEventType::MouseMoved, 0)?;
    let supported: Bool = unsafe { msg_send![web, respondsToSelector: sel!(_simulateMouseMove:)] };
    if !supported.as_bool() {
        return Err("this WebKit version cannot deliver native pointer motion".into());
    }
    unsafe {
        let _: () = msg_send![web, _simulateMouseMove: &*motion];
    }
    let count = match action {
        PointerAction::Hover => 0,
        PointerAction::Click => 1,
        PointerAction::DoubleClick => 2,
    };
    for count in 1..=count {
        for kind in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
            window.sendEvent(&*event(kind, count)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_keys_preserve_named_codes_and_shifted_positions() {
        assert_eq!(physical_key("Enter"), Some(("\r", 36)));
        assert_eq!(physical_key("a"), Some(("a", 0)));
        assert_eq!(physical_key("A"), Some(("A", 0)));
        assert_eq!(physical_key("!"), Some(("!", 18)));
        assert_eq!(physical_key("Tab"), Some(("\t", 48)));
        assert_eq!(physical_key("F12"), Some(("\u{f70f}", 111)));
        assert!(physical_key("한").is_none());
        assert!(physical_key("Unknown").is_none());
    }
}
