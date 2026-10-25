// SPDX-License-Identifier: GPL-3.0-or-later
//! Native editing confined to a process-owned WKWebView.
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Method, Sel};
use objc2::{msg_send, sel, ClassType, Message};
use objc2_app_kit::{NSEvent, NSEventModifierFlags as Flags, NSEventType, NSView};
use objc2_foundation::{NSNotFound, NSPoint, NSRange, NSString};
use objc2_web_kit::WKWebView;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicPtr, Ordering};

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
    if POINTER_SEQUENCE_ACTIVE.with(Cell::get) {
        return Err("native pointer input is still pending".into());
    }
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
    if POINTER_SEQUENCE_ACTIVE.with(Cell::get) {
        return Err("native pointer input is still pending".into());
    }
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

type MouseButtonsImp = unsafe extern "C-unwind" fn(&AnyClass, Sel) -> usize;
static ORIGINAL_MOUSE_BUTTONS: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
thread_local! {
    static POINTER_BUTTONS: Cell<Option<usize>> = const { Cell::new(None) };
}

unsafe extern "C-unwind" fn pointer_buttons(class: &AnyClass, selector: Sel) -> usize {
    if let Some(buttons) = POINTER_BUTTONS.with(Cell::get) {
        return buttons;
    }
    // Other threads retain the physical state while the main thread dispatches.
    let original: MouseButtonsImp =
        unsafe { std::mem::transmute(ORIGINAL_MOUSE_BUTTONS.load(Ordering::Acquire)) };
    unsafe { original(class, selector) }
}

struct PointerButtonsGuard<'a> {
    method: &'a Method,
    original: Imp,
}

impl Drop for PointerButtonsGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            self.method.set_implementation(self.original);
        }
        POINTER_BUTTONS.with(|buttons| buttons.set(None));
    }
}

fn with_pointer_buttons(buttons: usize, send: impl FnOnce()) -> Result<(), String> {
    let _mtm = objc2::MainThreadMarker::new().ok_or("pointer input requires the main thread")?;
    if POINTER_BUTTONS.with(Cell::get).is_some() {
        return Err("native pointer dispatch is already pending".into());
    }
    let method = NSEvent::class()
        .class_method(sel!(pressedMouseButtons))
        .ok_or("native pointer button state is unavailable")?;
    let original = method.implementation();
    ORIGINAL_MOUSE_BUTTONS.store(original as *mut (), Ordering::Release);
    POINTER_BUTTONS.with(|state| state.set(Some(buttons)));
    // WebKit reads this class method, which otherwise ignores generated NSEvents.
    let replacement: MouseButtonsImp = pointer_buttons;
    unsafe {
        method.set_implementation(std::mem::transmute::<MouseButtonsImp, Imp>(replacement));
    }
    let _restore = PointerButtonsGuard { method, original };
    send();
    Ok(())
}

// WebKit sends mouse events through NSTextInputContext asynchronously while an
// editor is focused. Enqueue the next event only after that completion forwarded
// the current event to WebKit; otherwise an immediate mouse-up can overtake down.
type InputCompletion = block2::DynBlock<dyn Fn(Bool)>;
type InputContextImp = unsafe extern "C-unwind" fn(&AnyObject, Sel, &NSEvent, &InputCompletion);
static ORIGINAL_INPUT_CONTEXT: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
fn after_native_input(next: Box<dyn FnOnce()>) {
    let next = RefCell::new(Some(next));
    let block = block2::RcBlock::new(move || {
        if let Some(next) = next.borrow_mut().take() {
            next();
        }
    });
    // The WebDriver process runs AppKit without a GLib loop. This queue is always
    // on the AppKit thread and also works in the embedded GTK browser.
    unsafe {
        let queue: Retained<AnyObject> = msg_send![objc2::class!(NSOperationQueue), mainQueue];
        let _: () = msg_send![&*queue, addOperationWithBlock: &*block];
    }
}

struct PendingMouseEvent {
    event: Retained<NSEvent>,
    next: Box<dyn FnOnce()>,
}
thread_local! {
    static PENDING_MOUSE_EVENT: RefCell<Option<PendingMouseEvent>> = const { RefCell::new(None) };
    static POINTER_SEQUENCE_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn pointer_pending() -> bool {
    POINTER_SEQUENCE_ACTIVE.with(Cell::get)
}

unsafe extern "C-unwind" fn input_context_event(
    context: &AnyObject,
    selector: Sel,
    event: &NSEvent,
    completion: &InputCompletion,
) {
    let original: InputContextImp =
        unsafe { std::mem::transmute(ORIGINAL_INPUT_CONTEXT.load(Ordering::Acquire)) };
    let pending = PENDING_MOUSE_EVENT.with(|pending| {
        let mut pending = pending.borrow_mut();
        if pending.as_ref().is_some_and(|pending| {
            pending.event.timestamp() == event.timestamp()
                && pending.event.windowNumber() == event.windowNumber()
                && pending.event.r#type() == event.r#type()
        }) {
            pending.take()
        } else {
            None
        }
    });
    let Some(pending) = pending else {
        unsafe { original(context, selector, event, completion) };
        return;
    };
    let completion = completion.copy();
    let next = RefCell::new(Some(pending.next));
    let buttons = usize::from(matches!(
        event.r#type(),
        NSEventType::LeftMouseDown | NSEventType::LeftMouseDragged
    ));
    let observed = block2::RcBlock::new(move |handled: Bool| {
        // A synchronous completion already runs under the dispatch's button guard.
        if POINTER_BUTTONS.with(Cell::get).is_some()
            || with_pointer_buttons(buttons, || completion.call((handled,))).is_err()
        {
            completion.call((handled,));
        }
        if let Some(next) = next.borrow_mut().take() {
            after_native_input(next);
        }
    });
    unsafe { original(context, selector, event, &observed) };
}

struct InputContextGuard {
    method: &'static Method,
    original: Imp,
}
impl Drop for InputContextGuard {
    fn drop(&mut self) {
        unsafe {
            self.method.set_implementation(self.original);
        }
    }
}

struct PointerSequence {
    web: Retained<WKWebView>,
    window: Retained<objc2_app_kit::NSWindow>,
    events: RefCell<std::vec::IntoIter<Retained<NSEvent>>>,
    context_method: &'static Method,
    pressed: Cell<bool>,
    requires_focus: bool,
    can_continue: Box<dyn Fn() -> bool>,
}
impl Drop for PointerSequence {
    fn drop(&mut self) {
        POINTER_SEQUENCE_ACTIVE.with(|active| active.set(false));
    }
}
impl PointerSequence {
    fn send_next(self: Rc<Self>) {
        let Some(mut event) = self.events.borrow_mut().next() else {
            return;
        };
        let view = self.web.as_super();
        if event.r#type() != NSEventType::LeftMouseUp
            && (!(self.can_continue)()
                || view.isHiddenOrHasHiddenAncestor()
                || view.window().as_ref() != Some(&self.window)
                || (self.requires_focus && !self.window.isKeyWindow()))
        {
            // A tab/window switch cancels remaining presses, but must release a held button.
            let release = self
                .pressed
                .get()
                .then(|| {
                    self.events
                        .borrow_mut()
                        .find(|event| event.r#type() == NSEventType::LeftMouseUp)
                })
                .flatten();
            *self.events.borrow_mut() = Vec::new().into_iter();
            match release {
                Some(up) => event = up,
                None => return,
            }
        }
        let original = self.context_method.implementation();
        ORIGINAL_INPUT_CONTEXT.store(original as *mut (), Ordering::Release);
        let next = self.clone();
        PENDING_MOUSE_EVENT.with(|pending| {
            *pending.borrow_mut() = Some(PendingMouseEvent {
                event: event.clone(),
                next: Box::new(move || next.send_next()),
            });
        });
        let replacement: InputContextImp = input_context_event;
        unsafe {
            self.context_method
                .set_implementation(std::mem::transmute::<InputContextImp, Imp>(replacement));
        }
        let restore = InputContextGuard {
            method: self.context_method,
            original,
        };
        let buttons = usize::from(matches!(
            event.r#type(),
            NSEventType::LeftMouseDown | NSEventType::LeftMouseDragged
        ));
        self.pressed.set(buttons != 0);
        let result = with_pointer_buttons(buttons, || {
            if event.r#type() == NSEventType::MouseMoved {
                unsafe {
                    let _: () = msg_send![&*self.web, _simulateMouseMove: &*event];
                }
            } else {
                self.window.sendEvent(&event);
            }
        });
        drop(restore);
        let pending = PENDING_MOUSE_EVENT.with(|pending| pending.borrow_mut().take());
        if let Err(error) = result {
            tracing::warn!(%error, "native pointer dispatch failed");
            return;
        }
        // Non-editable content has no input context and forwards synchronously.
        if let Some(pending) = pending {
            after_native_input(pending.next);
        }
    }
}

#[cfg(test)]
pub(crate) fn check_pointer_button_state() {
    let physical = NSEvent::pressedMouseButtons();
    let method = NSEvent::class()
        .class_method(sel!(pressedMouseButtons))
        .unwrap();
    let original = method.implementation() as usize;
    with_pointer_buttons(1, || {
        assert_eq!(
            NSEvent::pressedMouseButtons(),
            1,
            "native dispatch lost the held button"
        );
        assert!(with_pointer_buttons(0, || panic!("nested input must not run")).is_err());
        assert_eq!(NSEvent::pressedMouseButtons(), 1);
        assert_eq!(
            std::thread::spawn(NSEvent::pressedMouseButtons)
                .join()
                .unwrap(),
            physical,
            "synthetic buttons leaked to another thread"
        );
    })
    .unwrap();
    assert_eq!(NSEvent::pressedMouseButtons(), physical);
    with_pointer_buttons(0, || assert_eq!(NSEvent::pressedMouseButtons(), 0)).unwrap();
    assert_eq!(NSEvent::pressedMouseButtons(), physical);
    assert_eq!(
        method.implementation() as usize,
        original,
        "native button method was not restored"
    );
    println!("MACOS_POINTER_BUTTON_STATE_OK");
}

pub(crate) fn pointer(
    web: &WKWebView,
    action: flowmux_browser::input::PointerAction,
    point: NSPoint,
    can_continue: impl Fn() -> bool + 'static,
) -> Result<(), String> {
    use flowmux_browser::input::{PointerAction, PointerTarget};
    if POINTER_SEQUENCE_ACTIVE.with(Cell::get) {
        return Err("native pointer input is still pending".into());
    }
    let context_method = AnyClass::get(c"NSTextInputContext")
        .and_then(|class| class.instance_method(sel!(handleEvent:completionHandler:)))
        .ok_or("native input context completion is unavailable")?;
    if NSEvent::pressedMouseButtons() != 0 {
        return Err("release held pointer buttons before native input".into());
    }
    let view = web.as_super();
    let bounds = view.bounds();
    let window = view.window().ok_or("browser view is not attached")?;
    let to_window = |mut point: NSPoint| -> Result<NSPoint, String> {
        PointerTarget {
            x: point.x,
            y: point.y,
            width: bounds.size.width,
            height: bounds.size.height,
        }
        .scale(bounds.size.width, bounds.size.height)?;
        if !view.isFlipped() {
            point.y = bounds.size.height - point.y;
        }
        let content = window
            .contentView()
            .ok_or("browser has no native content view")?;
        let parent = unsafe { content.superview() };
        let hit = content.hitTest(view.convertPoint_toView(point, parent.as_deref()));
        if !hit.as_ref().is_some_and(|hit| hit.isDescendantOf(view)) {
            tracing::debug!(target = ?point, bounds = ?bounds, web_frame = ?view.frame(),
                content_frame = ?content.frame(), content_flipped = content.isFlipped(),
                hit = ?hit, "native browser pointer target is covered");
            return Err("browser pointer target is covered by a native view".into());
        }
        Ok(view.convertPoint_toView(point, None))
    };
    let drag_points = if let PointerAction::Drag { to_x, to_y } = action {
        (1..=16)
            .map(|step| {
                let fraction = f64::from(step) / 16.0;
                to_window(NSPoint::new(
                    point.x + (to_x - point.x) * fraction,
                    point.y + (to_y - point.y) * fraction,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let point = to_window(point)?;
    let event = |kind, count, point| {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind, point, Flags::empty(), gtk::glib::monotonic_time() as f64 / 1_000_000.0,
            window.windowNumber(), None, 0, count, 1.0,
        ).ok_or("could not create pointer event")
    };
    // WKWebView routes motion through its tracking owner, not NSResponder::mouseMoved.
    let motion = event(NSEventType::MouseMoved, 0, point)?;
    let supported: Bool = unsafe { msg_send![web, respondsToSelector: sel!(_simulateMouseMove:)] };
    if !supported.as_bool() {
        return Err("this WebKit version cannot deliver native pointer motion".into());
    }
    if !matches!(action, PointerAction::Hover) && !window.isKeyWindow() {
        // Otherwise AppKit can consume the first click to activate the window.
        window.makeKeyAndOrderFront(None);
        let mtm = objc2::MainThreadMarker::new().ok_or("pointer input requires the main thread")?;
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
    let mut events = vec![motion];
    if let Some(end) = drag_points.last() {
        events.push(event(NSEventType::LeftMouseDown, 1, point)?);
        for point in &drag_points {
            events.push(event(NSEventType::LeftMouseDragged, 0, *point)?);
        }
        events.push(event(NSEventType::LeftMouseUp, 1, *end)?);
    } else {
        let count = match action {
            PointerAction::Hover => 0,
            PointerAction::Click => 1,
            PointerAction::DoubleClick => 2,
            PointerAction::Drag { .. } => unreachable!(),
        };
        for count in 1..=count {
            events.push(event(NSEventType::LeftMouseDown, count, point)?);
            events.push(event(NSEventType::LeftMouseUp, count, point)?);
        }
    }
    POINTER_SEQUENCE_ACTIVE.with(|active| active.set(true));
    let sequence = Rc::new(PointerSequence {
        web: web.retain(),
        window,
        events: RefCell::new(events.into_iter()),
        context_method,
        pressed: Cell::new(false),
        requires_focus: !matches!(action, PointerAction::Hover),
        can_continue: Box::new(can_continue),
    });
    // Give AppKit's activation request a turn before checking native window focus.
    after_native_input(Box::new(move || sequence.send_next()));
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
