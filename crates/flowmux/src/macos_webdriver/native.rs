// SPDX-License-Identifier: GPL-3.0-or-later
//! AppKit input confined to this process's ephemeral automation windows.
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{msg_send, sel, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask,
    NSEventModifierFlags, NSEventType, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration, WKWebsiteDataStore};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub(super) fn application() -> Retained<NSApplication> {
    let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app
}

pub(super) fn pump() {
    let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
    unsafe {
        let until = NSDate::dateWithTimeIntervalSinceNow(0.01);
        if let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&until),
            NSDefaultRunLoopMode,
            true,
        ) {
            app.sendEvent(&event);
        }
        app.updateWindows();
    }
}

pub(super) fn wait<T>(result: &RefCell<Option<T>>, timeout: Duration) -> Result<T, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = result.borrow_mut().take() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err("native browser operation timed out".into());
        }
        pump();
    }
}

pub(super) struct Window {
    pub window: Retained<NSWindow>,
    pub web: Retained<WKWebView>,
    pub registry: String,
    navigation: Cell<usize>,
    pub committed: Cell<bool>,
    pub navigation_error: RefCell<Option<String>>,
}

impl Window {
    pub fn new(configuration: &WKWebViewConfiguration) -> Self {
        let mtm = MainThreadMarker::new().unwrap();
        let frame = NSRect::new(NSPoint::new(80.0, 80.0), NSSize::new(900.0, 600.0));
        unsafe {
            configuration.preferences().setTabFocusesLinks(true);
            let web =
                WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, configuration);
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window.setTitle(&NSString::from_str("Flowmux · WebDriver"));
            window.setContentView(Some(&web));
            window.makeKeyAndOrderFront(None);
            Self {
                window,
                web,
                registry: format!("__flowmux_{}", uuid::Uuid::new_v4().simple()),
                navigation: Cell::new(0),
                committed: Cell::new(true),
                navigation_error: RefCell::new(None),
            }
        }
    }

    #[cfg(test)]
    pub fn evaluate(&self, expression: &str) -> Result<serde_json::Value, String> {
        self.evaluate_timeout(expression, Duration::from_secs(10))
    }

    pub fn evaluate_timeout(
        &self,
        expression: &str,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let result = Rc::new(RefCell::new(None));
        let done = result.clone();
        let callback = RcBlock::new(
            move |value: *mut objc2::runtime::AnyObject, error: *mut objc2_foundation::NSError| {
                let response = unsafe {
                    if let Some(error) = error.as_ref() {
                        Err(error.localizedDescription().to_string())
                    } else if let Some(string) = value
                        .as_ref()
                        .and_then(|value| value.downcast_ref::<NSString>())
                    {
                        serde_json::from_str(&string.to_string()).map_err(|error| error.to_string())
                    } else {
                        Ok(serde_json::Value::Null)
                    }
                };
                *done.borrow_mut() = Some(response);
            },
        );
        unsafe {
            self.web.evaluateJavaScript_completionHandler(
                &NSString::from_str(&format!("JSON.stringify({expression})")),
                Some(&callback),
            );
        }
        wait(&result, timeout)?
    }

    fn activate(&self) -> Result<(), String> {
        if self.window.isKeyWindow() {
            return Ok(());
        }
        self.window.makeKeyAndOrderFront(None);
        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.window.isKeyWindow() {
            if Instant::now() >= deadline {
                return Err("Automation window could not acquire native focus".into());
            }
            pump();
        }
        Ok(())
    }

    pub fn click(&self, point: NSPoint) -> Result<(), String> {
        let bounds = self.web.as_super().bounds();
        flowmux_browser::input::PointerTarget {
            x: point.x,
            y: point.y,
            width: bounds.size.width,
            height: bounds.size.height,
        }
        .scale(bounds.size.width, bounds.size.height)?;
        self.activate()?;
        crate::macos_input::pointer(
            &self.web,
            flowmux_browser::input::PointerAction::Click,
            point,
            || true,
        )?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while crate::macos_input::pointer_pending() {
            if Instant::now() >= deadline {
                return Err("native pointer input timed out".into());
            }
            pump();
        }
        Ok(())
    }

    pub fn text(&self, text: &str) -> Result<(), String> {
        crate::macos_input::insert_text(&self.web, text)
    }

    pub fn key(
        &self,
        text: &str,
        code: u16,
        down: bool,
        flags: NSEventModifierFlags,
    ) -> Result<(), String> {
        self.activate()?;
        let shifted;
        let text = if flags.contains(NSEventModifierFlags::Shift) && text.len() == 1 {
            let plain = "`1234567890-=[]\\;',./";
            let upper = "~!@#$%^&*()_+{}|:\"<>?";
            shifted = plain
                .find(text)
                .map(|i| upper[i..i + 1].to_string())
                .unwrap_or_else(|| text.to_ascii_uppercase());
            &shifted
        } else {
            text
        };
        let chars = NSString::from_str(text);
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            if down { NSEventType::KeyDown } else { NSEventType::KeyUp }, NSPoint::new(0.0, 0.0),
            flags, timestamp(), self.window.windowNumber(), None, &chars, &chars, false, code,
        ).ok_or("could not create keyboard event")?;
        if !down
            || !flags.contains(NSEventModifierFlags::Command)
            || !self.web.performKeyEquivalent(&event)
        {
            self.window.sendEvent(&event);
        }
        Ok(())
    }

    pub fn end(&self) -> Result<(), String> {
        let responder = self
            .window
            .firstResponder()
            .ok_or("browser has no keyboard focus")?;
        if !unsafe { responder.tryToPerform_with(sel!(moveToEndOfDocument:), None) } {
            return Err("Focused browser cannot move the caret".into());
        }
        Ok(())
    }

    pub fn clear(&self) -> Result<(), String> {
        let responder = self
            .window
            .firstResponder()
            .filter(|responder| {
                responder
                    .downcast_ref::<NSView>()
                    .is_some_and(|view| view.isDescendantOf(self.web.as_super()))
            })
            .ok_or("browser does not own keyboard focus")?;
        for action in [sel!(selectAll:), sel!(deleteBackward:)] {
            if !unsafe { responder.tryToPerform_with(action, None) } {
                return Err("Focused browser cannot perform editing command".into());
            }
        }
        Ok(())
    }
}

fn timestamp() -> f64 {
    gtk::glib::monotonic_time() as f64 / 1_000_000.0
}

impl Drop for Window {
    fn drop(&mut self) {
        self.window.close();
    }
}

#[cfg(test)]
pub(crate) fn smoke() {
    let _app = application();
    crate::macos_input::check_pointer_button_state();
    let mtm = MainThreadMarker::new().unwrap();
    let config = unsafe { WKWebViewConfiguration::new(mtm) };
    unsafe {
        config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
    }
    let browser = Window::new(&config);
    let view = browser.web.as_super();
    let original_frame = view.frame();
    view.setFrame(NSRect::new(
        NSPoint::new(40.0, 40.0),
        NSSize::new(800.0, 500.0),
    ));
    crate::macos_input::pointer(
        &browser.web,
        flowmux_browser::input::PointerAction::Hover,
        NSPoint::new(5.0, 5.0),
        || true,
    )
    .expect("hit testing must use the content view's parent coordinates");
    view.setFrame(original_frame);
    println!("MACOS_POINTER_HIT_TEST_OK");
    unsafe {
        browser.web.loadHTMLString_baseURL(&NSString::from_str(
        "<form><input id=field><button id=submit>Submit</button></form><script>window.events=[];window.submissions=0;for(const name of ['pointerdown','mousedown','click','keydown','input'])document.addEventListener(name,e=>events.push([e.type,e.isTrusted]));document.querySelector('form').onsubmit=e=>{e.preventDefault();submissions++}</script>"
    ), None);
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while browser
        .evaluate("!!document.querySelector('#field')")
        .unwrap_or_default()
        != true
    {
        assert!(Instant::now() < deadline);
        pump();
    }
    browser.evaluate("(()=>{window.pointerReady=false;requestAnimationFrame(()=>requestAnimationFrame(()=>window.pointerReady=true));return true})()").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while browser.evaluate("pointerReady").unwrap() != true {
        assert!(
            Instant::now() < deadline,
            "browser did not present the pointer fixture"
        );
        pump();
    }
    browser
        .evaluate("field.onpointermove=e=>{window.hoverTrusted=e.isTrusted;window.hoverDuringMove=field.matches(':scope:hover')}")
        .unwrap();
    crate::macos_input::pointer(
        &browser.web,
        flowmux_browser::input::PointerAction::Hover,
        NSPoint::new(30.0, 15.0),
        || true,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while browser
        .evaluate("window.hoverTrusted === true && field.matches(':scope:hover')")
        .unwrap()
        != true
    {
        assert!(
            Instant::now() < deadline,
            "native hover did not reach input: {}",
            browser.evaluate("JSON.stringify({trusted:window.hoverTrusted,hoverDuringMove:window.hoverDuringMove,hover:field.matches(':scope:hover'),focus:document.hasFocus(),rect:field.getBoundingClientRect()})").unwrap()
        );
        pump();
    }
    browser.click(NSPoint::new(30.0, 15.0)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if browser.evaluate("document.activeElement.id").unwrap() == "field" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native click did not focus input"
        );
        pump();
    }
    browser.text("한글 café").unwrap();
    // A JS round-trip also waits for the preceding native input in WebKit's process.
    assert_eq!(browser.evaluate("field.value").unwrap(), "한글 café");
    for (text, code) in [("\u{7f}", 51), ("\r", 36), ("\t", 48)] {
        for down in [true, false] {
            browser
                .key(text, code, down, NSEventModifierFlags::empty())
                .unwrap();
        }
        let _ = browser.evaluate("0").unwrap();
    }
    assert_eq!(browser.evaluate("field.value").unwrap(), "한글 caf");
    assert_eq!(browser.evaluate("submissions").unwrap(), 1);
    assert_eq!(
        browser.evaluate("document.activeElement.id").unwrap(),
        "submit"
    );
    let events: Vec<(String, bool)> =
        serde_json::from_value(browser.evaluate("events").unwrap()).unwrap();
    for name in ["pointerdown", "mousedown", "click", "keydown", "input"] {
        assert!(
            events.contains(&(name.into(), true)),
            "missing trusted {name}: {events:?}"
        );
    }
    assert!(events.iter().all(|(_, trusted)| *trusted));
    browser
        .evaluate("(()=>{window.releases=0;field.onpointerup=()=>releases++;return true})()")
        .unwrap();
    let input_context = objc2::runtime::AnyClass::get(c"NSTextInputContext")
        .unwrap()
        .instance_method(sel!(handleEvent:completionHandler:))
        .unwrap();
    let original = input_context.implementation() as usize;
    let steps = Cell::new(0);
    crate::macos_input::pointer(
        &browser.web,
        flowmux_browser::input::PointerAction::Drag {
            to_x: 100.0,
            to_y: 15.0,
        },
        NSPoint::new(30.0, 15.0),
        move || {
            steps.set(steps.get() + 1);
            steps.get() <= 2
        },
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while crate::macos_input::pointer_pending() {
        assert!(
            Instant::now() < deadline,
            "cancelled pointer input did not settle"
        );
        pump();
    }
    assert_eq!(browser.evaluate("releases").unwrap(), 1);
    assert_eq!(input_context.implementation() as usize, original);
    assert_eq!(NSEvent::pressedMouseButtons(), 0);
    println!("MACOS_POINTER_CANCEL_RELEASE_OK");
    let other = Window::new(&config);
    unsafe {
        other.web.loadHTMLString_baseURL(&NSString::from_str(
            "<input id=field value=safe><script>window.hits=0;document.onclick=()=>hits++</script>"
        ), None);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while other
        .evaluate("!!document.querySelector('#field')")
        .unwrap_or_default()
        != true
    {
        assert!(Instant::now() < deadline);
        pump();
    }
    other.activate().unwrap();
    assert!(other.window.isKeyWindow());
    assert!(!browser.window.isKeyWindow());
    let dispatch_started = Rc::new(Cell::new(false));
    let dispatched = dispatch_started.clone();
    // The production pointer path addresses its own NSWindow while another is key.
    crate::macos_input::pointer(
        &browser.web,
        flowmux_browser::input::PointerAction::Click,
        NSPoint::new(30.0, 15.0),
        move || {
            dispatched.set(true);
            true
        },
    )
    .unwrap();
    assert!(
        !dispatch_started.get(),
        "pointer dispatch must let AppKit settle window activation first"
    );
    assert!(browser.window.isKeyWindow());
    let deadline = Instant::now() + Duration::from_secs(5);
    while crate::macos_input::pointer_pending()
        || browser.evaluate("document.activeElement.id").unwrap() != "field"
    {
        assert!(
            Instant::now() < deadline,
            "pointer did not focus its own window"
        );
        pump();
    }
    assert!(dispatch_started.get());
    crate::macos_input::press_key(
        &browser.web,
        &flowmux_browser::input::KeyStroke::parse("End").unwrap(),
    )
    .unwrap();
    browser.text("!").unwrap();
    assert!(browser.evaluate("field.value.includes('!')").unwrap() == true);
    assert_eq!(other.evaluate("field.value").unwrap(), "safe");
    assert_eq!(other.evaluate("hits").unwrap(), 0);
    println!("MACOS_POINTER_WINDOW_ISOLATION_OK");
    println!("MACOS_WEBDRIVER_NATIVE_INPUT_OK");
}

use objc2::{define_class, AnyThread, DefinedClass};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSHTTPCookie, NSObject, NSObjectProtocol};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationDelegate, WKSnapshotConfiguration, WKUIDelegate,
    WKWindowFeatures,
};
use std::collections::BTreeMap;
use std::rc::Weak;
type Windows = RefCell<BTreeMap<String, Rc<Window>>>;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Weak<Windows>]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl WKNavigationDelegate for Delegate {
        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn started(&self, web: &WKWebView, navigation: Option<&WKNavigation>) {
            if let Some(window) = self.browser(web) {
                window.navigation.set(navigation.map_or(0, |n| n as *const _ as usize));
                window.committed.set(false);
                *window.navigation_error.borrow_mut() = None;
            }
        }
        #[unsafe(method(webView:didCommitNavigation:))]
        fn committed(&self, web: &WKWebView, _: Option<&WKNavigation>) {
            if let Some(window) = self.browser(web) { window.committed.set(true); }
        }
        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn provisional_failed(&self, web: &WKWebView, navigation: Option<&WKNavigation>, error: &NSError) {
            self.record_failure(web, navigation, error);
        }
        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed(&self, web: &WKWebView, navigation: Option<&WKNavigation>, error: &NSError) {
            self.record_failure(web, navigation, error);
        }
    }
    unsafe impl WKUIDelegate for Delegate {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn popup(&self, _: &WKWebView, configuration: &WKWebViewConfiguration,
            _: &WKNavigationAction, _: &WKWindowFeatures) -> Option<Retained<WKWebView>> {
            self.ivars().upgrade().map(|windows| {
            let browser = Rc::new(Window::new(configuration));
            unsafe { browser.web.setUIDelegate(Some(objc2::runtime::ProtocolObject::from_ref(self)));
                browser.web.setNavigationDelegate(Some(objc2::runtime::ProtocolObject::from_ref(self))); }
            let web = browser.web.clone();
            windows.borrow_mut().insert(uuid::Uuid::new_v4().to_string(), browser);
            web
            })
        }
        #[unsafe(method(webViewDidClose:))]
        fn closed(&self, web: &WKWebView) {
            if let Some(windows) = self.ivars().upgrade() {
                windows.borrow_mut().retain(|_, window| &*window.web != web);
            }
        }
    }
);

impl Delegate {
    fn record_failure(&self, web: &WKWebView, navigation: Option<&WKNavigation>, error: &NSError) {
        if let Some(window) = self.browser(web) {
            if window.navigation.get() == navigation.map_or(0, |n| n as *const _ as usize) {
                *window.navigation_error.borrow_mut() =
                    Some(error.localizedDescription().to_string());
            }
        }
    }
    fn browser(&self, web: &WKWebView) -> Option<Rc<Window>> {
        self.ivars()
            .upgrade()?
            .borrow()
            .values()
            .find(|window| &*window.web == web)
            .cloned()
    }
}

pub(super) struct Browsers {
    pub windows: Rc<Windows>,
    delegate: Retained<Delegate>,
    pub store: Retained<WKWebsiteDataStore>,
}
impl Browsers {
    pub fn new() -> Self {
        let mtm = MainThreadMarker::new().unwrap();
        let windows = Rc::new(RefCell::new(BTreeMap::new()));
        let delegate = unsafe {
            msg_send![
                super(Delegate::alloc(mtm).set_ivars(Rc::downgrade(&windows))),
                init
            ]
        };
        Self {
            windows,
            delegate,
            store: unsafe { WKWebsiteDataStore::nonPersistentDataStore(mtm) },
        }
    }
    pub fn open(&self) -> String {
        let config = unsafe { WKWebViewConfiguration::new(MainThreadMarker::new().unwrap()) };
        unsafe {
            config.setWebsiteDataStore(&self.store);
        }
        let browser = Rc::new(Window::new(&config));
        unsafe {
            browser
                .web
                .setUIDelegate(Some(objc2::runtime::ProtocolObject::from_ref(
                    &*self.delegate,
                )));
            browser
                .web
                .setNavigationDelegate(Some(objc2::runtime::ProtocolObject::from_ref(
                    &*self.delegate,
                )));
        }
        let handle = uuid::Uuid::new_v4().to_string();
        self.windows.borrow_mut().insert(handle.clone(), browser);
        handle
    }
    pub fn cookies(&self) -> Result<Vec<Retained<NSHTTPCookie>>, String> {
        let result = Rc::new(RefCell::new(None));
        let done = result.clone();
        let callback = RcBlock::new(move |cookies: std::ptr::NonNull<NSArray<NSHTTPCookie>>| {
            let cookies = unsafe { cookies.as_ref() };
            *done.borrow_mut() = Some(cookies.to_vec());
        });
        unsafe {
            self.store.httpCookieStore().getAllCookies(&callback);
        }
        wait(&result, Duration::from_secs(10))
    }
}

impl Window {
    pub fn screenshot(&self) -> Result<String, String> {
        use base64::Engine;
        use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
        let result = Rc::new(RefCell::new(None));
        let done = result.clone();
        let callback = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            let encode = || unsafe {
                if let Some(error) = error.as_ref() {
                    return Err(error.localizedDescription().to_string());
                }
                let tiff = image
                    .as_ref()
                    .and_then(|image| image.TIFFRepresentation())
                    .ok_or("empty screenshot")?;
                let bitmap = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &tiff)
                    .ok_or("invalid screenshot")?;
                let png = bitmap
                    .representationUsingType_properties(
                        NSBitmapImageFileType::PNG,
                        &NSDictionary::new(),
                    )
                    .ok_or("PNG encoding failed")?;
                Ok(base64::engine::general_purpose::STANDARD.encode(png.to_vec()))
            };
            *done.borrow_mut() = Some(encode());
        });
        unsafe {
            self.web.takeSnapshotWithConfiguration_completionHandler(
                None::<&WKSnapshotConfiguration>,
                &callback,
            );
        }
        wait(&result, Duration::from_secs(10))?
    }
}
