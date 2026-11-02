// SPDX-License-Identifier: GPL-3.0-or-later
//! Synthetic NSEvents traverse the real macOS GTK/VTE input path in an isolated window.
use super::*;
use gtk::glib::translate::ToGlibPtr;
use objc2::{define_class, msg_send, rc::Retained, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventPhase,
    NSEventType, NSWindow,
};
use objc2_foundation::NSPoint;
use vte::prelude::*;

#[repr(C)]
struct CGEvent {
    _private: [u8; 0],
}
unsafe impl objc2::encode::RefEncode for CGEvent {
    const ENCODING_REF: objc2::encode::Encoding =
        objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("__CGEvent", &[]));
}

struct WheelState {
    window: Retained<NSWindow>,
    point: NSPoint,
    delta: f64,
    precise: bool,
    cg: *const CGEvent,
}
impl Drop for WheelState {
    fn drop(&mut self) {
        unsafe { CFRelease(self.cg.cast()) }
    }
}
define_class!(
    #[unsafe(super(NSEvent))]
    #[thread_kind = MainThreadOnly]
    #[ivars = WheelState]
    struct FlowmuxTestWheelEvent;
    impl FlowmuxTestWheelEvent {
        #[unsafe(method(CGEvent))]
        fn cg(&self) -> *const CGEvent { self.ivars().cg }
        #[unsafe(method(type))]
        fn kind(&self) -> NSEventType { NSEventType::ScrollWheel }
        #[unsafe(method(window))]
        fn window(&self) -> *const NSWindow { &*self.ivars().window }
        #[unsafe(method(windowNumber))]
        fn number(&self) -> isize { self.ivars().window.windowNumber() }
        #[unsafe(method(locationInWindow))]
        fn location(&self) -> NSPoint { self.ivars().point }
        #[unsafe(method(timestamp))]
        fn timestamp(&self) -> f64 { glib::monotonic_time() as f64 / 1_000_000.0 }
        #[unsafe(method(modifierFlags))]
        fn modifiers(&self) -> NSEventModifierFlags { NSEventModifierFlags::empty() }
        #[unsafe(method(deltaX))]
        fn dx(&self) -> f64 { 0.0 }
        #[unsafe(method(deltaY))]
        fn dy(&self) -> f64 { self.ivars().delta }
        #[unsafe(method(scrollingDeltaX))]
        fn sx(&self) -> f64 { 0.0 }
        #[unsafe(method(scrollingDeltaY))]
        fn sy(&self) -> f64 { self.ivars().delta }
        #[unsafe(method(hasPreciseScrollingDeltas))]
        fn precise(&self) -> objc2::runtime::Bool { self.ivars().precise.into() }
        #[unsafe(method(phase))]
        fn phase(&self) -> NSEventPhase { NSEventPhase::empty() }
        #[unsafe(method(momentumPhase))]
        fn momentum(&self) -> NSEventPhase { NSEventPhase::empty() }
        #[unsafe(method(isDirectionInvertedFromDevice))]
        fn inverted(&self) -> objc2::runtime::Bool { objc2::runtime::Bool::NO }
    }
);

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateScrollWheelEvent(
        source: *const std::ffi::c_void,
        units: u32,
        count: u32,
        wheel: i32,
        ...
    ) -> *const std::ffi::c_void;
    fn CFRelease(value: *const std::ffi::c_void);
    fn gdk_macos_surface_get_native_window(
        surface: *mut gtk::gdk::ffi::GdkSurface,
    ) -> *mut std::ffi::c_void;
}

fn wheel(window: &gtk::Window, term: &vte::Terminal, delta: f64, precise: bool) {
    let mtm = MainThreadMarker::new().unwrap();
    let surface = window.surface().unwrap();
    let native = unsafe {
        &*gdk_macos_surface_get_native_window(surface.to_glib_none().0).cast::<NSWindow>()
    };
    let point = term
        .compute_point(window, &gtk::graphene::Point::new(30.0, 30.0))
        .unwrap();
    unsafe {
        let cg =
            CGEventCreateScrollWheelEvent(std::ptr::null(), u32::from(!precise), 1, delta as i32);
        assert!(!cg.is_null());
        let event = FlowmuxTestWheelEvent::alloc(mtm).set_ivars(WheelState {
            window: Retained::retain(native as *const NSWindow as *mut NSWindow).unwrap(),
            point: NSPoint::new(
                f64::from(point.x()),
                native.contentView().unwrap().frame().size.height - f64::from(point.y()),
            ),
            delta,
            precise,
            cg: cg.cast(),
        });
        let event: Retained<FlowmuxTestWheelEvent> = msg_send![super(event), init];
        assert_eq!(event.windowNumber(), native.windowNumber());
        let app = NSApplication::sharedApplication(mtm);
        let motion = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::MouseMoved, event.locationInWindow(), NSEventModifierFlags::empty(), glib::monotonic_time() as f64 / 1_000_000.0, native.windowNumber(), None, 0, 0, 0.0,
        ).unwrap();
        app.postEvent_atStart(&motion, false);
        app.postEvent_atStart(&event, false);
    }
}

pub(super) async fn check(app: &adw::Application) {
    let native_app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
    native_app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    native_app.finishLaunching();
    #[allow(deprecated)]
    native_app.activateIgnoringOtherApps(true);
    let store = StateStore::new_lazy_ephemeral(State::default());
    let workspace = store
        .create_workspace(Some("Wheel test".into()), std::env::temp_dir())
        .await;
    let ws = store.get_workspace(workspace).await.unwrap();
    let (bridge, rx) = Bridge::new();
    let controller = WindowController::new(
        app,
        store,
        Arc::new(ResolvedTheme::load()),
        bridge,
        gtk::CssProvider::new(),
        None,
    );
    controller.render_workspace(&ws);
    spawn_dispatch_loop(rx, controller.clone());
    controller.window.present();
    let pane = controller
        .pane_registry
        .borrow()
        .terminals
        .values()
        .next()
        .unwrap()
        .clone();
    let term = &pane.widget;
    wait_until("wheel test terminal mapped", || term.is_mapped()).await;
    term.feed("fixture row\r\n".repeat(1000).as_bytes());
    let adjustment = term.vadjustment().unwrap();
    wait_until("wheel test history ready", || adjustment.upper() > 800.0).await;

    let dialog = options_merge::dialog(&controller).await;
    let spin = options_merge::control(&dialog, "Terminal mouse wheel speed (×)")
        .downcast::<gtk::SpinButton>()
        .unwrap();
    spin.set_value(2.5);
    wait_until("wheel speed saved", || {
        flowmux_config::options::load().terminal_wheel_scroll_multiplier == 2.5
    })
    .await;
    dialog.close();
    wait_until("wheel options closed", || !dialog.is_visible()).await;
    let base = (term.row_count() as f64 / 10.0).ceil().max(1.0);
    for (factor, delta, precise) in [
        (2.5, -1.0, false),
        (1.0, -1.0, false),
        (2.0, 1.0, false),
        (0.5, -1.0, false),
        (5.0, -1.0, false),
        (1.0, -1.0, true),
        (5.0, -1.0, true),
    ] {
        // The first case uses the setting applied by the real options dialog.
        if factor != 2.5 {
            pane.set_wheel_scroll_multiplier(factor);
        }
        adjustment.set_value(200.0);
        glib::timeout_future(Duration::from_millis(30)).await;
        wheel(controller.window.upcast_ref(), term, delta, precise);
        wait_until("native wheel moved terminal", || {
            adjustment.value() != 200.0
        })
        .await;
        let expected = 200.0 - delta * base * if precise { 1.0 } else { factor };
        assert!(
            (adjustment.value() - expected).abs() < 0.01,
            "factor {factor}, precise {precise}: {} != {expected}",
            adjustment.value()
        );
        println!(
            "WHEEL_SCROLL factor={factor} precise={precise} rows={}",
            adjustment.value() - 200.0
        );
    }
    let dialog = options_merge::dialog(&controller).await;
    assert_eq!(
        options_merge::control(&dialog, "Terminal mouse wheel speed (×)")
            .downcast::<gtk::SpinButton>()
            .unwrap()
            .value(),
        2.5
    );
    dialog.close();
    let new_pane = controller
        .split_pane(
            ws.surfaces[0].root_pane.first_leaf_id().unwrap(),
            SplitDirection::Vertical,
        )
        .await
        .unwrap();
    let new_terminal = controller
        .pane_registry
        .borrow()
        .active_terminal(new_pane)
        .unwrap()
        .clone();
    let new_term = &new_terminal.widget;
    wait_until("new terminal mapped", || new_term.is_mapped()).await;
    new_term.feed("new terminal row\r\n".repeat(1000).as_bytes());
    let new_adjustment = new_term.vadjustment().unwrap();
    wait_until("new terminal history ready", || {
        new_adjustment.upper() > 800.0
    })
    .await;
    new_adjustment.set_value(200.0);
    glib::timeout_future(Duration::from_millis(30)).await;
    wheel(controller.window.upcast_ref(), new_term, -1.0, false);
    wait_until("new terminal wheel moved", || {
        new_adjustment.value() != 200.0
    })
    .await;
    let expected = 200.0 + (new_term.row_count() as f64 / 10.0).ceil() * 2.5;
    assert!((new_adjustment.value() - expected).abs() < 0.01);
    check_mouse_protocol(controller.window.upcast_ref(), &pane).await;
    controller.window.destroy();
    println!("MACOS_TERMINAL_WHEEL_SCROLL_OK");
}

async fn check_mouse_protocol(window: &gtk::Window, pane: &crate::ui::pane_terminal::PaneTerminal) {
    let term = &pane.widget;
    term.vadjustment()
        .unwrap()
        .set_value(term.vadjustment().unwrap().upper());
    let sent = Rc::new(RefCell::new(String::new()));
    term.connect_commit(glib::clone!(
        #[strong]
        sent,
        move |_, text, _| sent.borrow_mut().push_str(text)
    ));

    term.feed(b"\x1b[?1000h\x1b[?1006h\x1b[?1000$p");
    wait_until("mouse mode enabled", || sent.borrow().contains("?1000;1$y")).await;
    sent.borrow_mut().clear();
    pane.set_wheel_scroll_multiplier(2.0);
    wheel(window, term, -1.0, false);
    wait_until("two mouse wheel reports", || {
        sent.borrow().matches('\x1b').count() >= 2
    })
    .await;
    let report = sent.borrow().clone();
    assert_eq!(report.matches("\x1b[<65;").count(), 2, "{report:?}");
    sent.borrow_mut().clear();
    pane.set_wheel_scroll_multiplier(0.5);
    wheel(window, term, 1.0, false);
    glib::timeout_future(Duration::from_millis(100)).await;
    assert!(sent.borrow().is_empty(), "fractional wheel must accumulate");
    wheel(window, term, 1.0, false);
    wait_until("fractional wheel report", || !sent.borrow().is_empty()).await;
    assert_eq!(sent.borrow().matches("\x1b[<64;").count(), 1);
    term.feed(b"\x1b[?1000l\x1b[?1006l\x1b[?1049h\x1b[?1007h");
    glib::timeout_future(Duration::from_millis(100)).await;
    sent.borrow_mut().clear();
    pane.set_wheel_scroll_multiplier(2.0);
    wheel(window, term, -1.0, false);
    wait_until("alternate screen wheel keys", || !sent.borrow().is_empty()).await;
    let count = ((term.row_count() as f64 / 10.0).ceil() * 2.0) as usize;
    assert_eq!(*sent.borrow(), "\x1b[B".repeat(count));
}
