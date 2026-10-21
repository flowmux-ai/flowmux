// SPDX-License-Identifier: GPL-3.0-or-later
//! Native windowing only; parsing and the isolated Mermaid renderer are shared with Linux.
use super::{Args, FileSignature, RenderOptions, MARKDOWN_SCRIPT_WORLD, RENDER_TIMEOUT_SECS};
use block2::RcBlock;
use flowmux_md_viewer::render_markdown_file;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSButton, NSEventMask,
    NSEventModifierFlags, NSImage, NSMenu, NSMenuItem, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSDictionary, NSError, NSObject, NSPoint, NSRect, NSSize,
    NSString, NSURLRequest, NSURL,
};
use objc2_web_kit::{
    WKContentWorld, WKUserScript, WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration,
    WKWebsiteDataStore,
};
use std::cell::RefCell;
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

mod resources;
mod theme;

#[cfg(test)]
pub(super) mod smoke;

struct State {
    args: Args,
    web: Retained<WKWebView>,
    signature: Option<FileSignature>,
    resources: Retained<resources::Resources>,
    theme: Option<theme::Theme>,
}

impl State {
    fn reload(&self) {
        let (html, base) = match render_markdown_file(&self.args.path, &self.args.options) {
            Ok(document) => (document.html, document.base_dir),
            Err(error) => (super::error_html(&error.to_string()), None),
        };
        let html = match &self.theme {
            Some(theme) => theme.document(html),
            None => html,
        };
        self.resources.set_document(html, base);
        let base =
            NSURL::URLWithString(&NSString::from_str("flowmux-markdown://document/")).unwrap();
        unsafe {
            self.web.loadRequest(&NSURLRequest::requestWithURL(&base));
        }
    }

    fn poll(&mut self) {
        if !self.args.watch {
            return;
        }
        let signature = super::file_signature(&self.args.path);
        if signature != self.signature {
            self.signature = signature;
            self.reload();
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "FlowmuxMarkdownControls"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Rc<RefCell<State>>]
    struct Controls;

    impl Controls {
        #[unsafe(method(activate:))]
        fn activate(&self, sender: &NSButton) {
            let mut state = self.ivars().borrow_mut();
            unsafe {
                match sender.tag() {
                    0 if state.web.canGoBack() => { state.web.goBack(); }
                    0 | 2 => state.reload(),
                    1 => { state.web.goForward(); }
                    tag => {
                        let zoom = state.args.options.normalized_zoom();
                        state.args.options.zoom = match tag {
                            3 => zoom / 1.15,
                            5 => zoom * 1.15,
                            _ => 1.0,
                        }.clamp(0.25, 4.0);
                        state.web.setPageZoom(state.args.options.normalized_zoom() as f64);
                    }
                }
            }
        }
    }
);

struct Viewer {
    app: Retained<NSApplication>,
    window: Retained<NSWindow>,
    state: Rc<RefCell<State>>,
    // Cocoa action targets are weak references.
    _controls: Retained<Controls>,
}

impl Viewer {
    fn new(args: Args, visible: bool) -> Self {
        let mtm = MainThreadMarker::new().expect("Markdown viewer must run on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(if visible {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        });
        app.finishLaunching();
        let size = NSSize::new(
            args.options.normalized_width() as f64,
            args.window_height as f64,
        );
        let chrome = if visible { 40.0 } else { 0.0 };
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
        unsafe {
            let configuration = WKWebViewConfiguration::new(mtm);
            let resources = resources::Resources::new(mtm);
            configuration.setURLSchemeHandler_forURLScheme(
                Some(ProtocolObject::from_ref(&*resources)),
                &NSString::from_str("flowmux-markdown"),
            );
            configuration.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
            configuration
                .defaultWebpagePreferences()
                .setAllowsContentJavaScript(false);
            let world =
                WKContentWorld::worldWithName(&NSString::from_str(MARKDOWN_SCRIPT_WORLD), mtm);
            let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                WKUserScript::alloc(mtm),
                &NSString::from_str(include_str!(
                    "../../../editor/flowmux-editor-web/dist/markdown.js"
                )),
                WKUserScriptInjectionTime::AtDocumentEnd,
                true,
                &world,
            );
            configuration.userContentController().addUserScript(&script);
            let web = WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                NSRect::new(frame.origin, NSSize::new(size.width, size.height - chrome)),
                &configuration,
            );
            web.setPageZoom(args.options.normalized_zoom() as f64);
            web.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window.setTitle(&NSString::from_str(&format!(
                "{} — Flowmux Markdown",
                args.path.file_name().unwrap_or_default().to_string_lossy()
            )));
            let root = NSView::initWithFrame(NSView::alloc(mtm), frame);
            root.addSubview(&web);
            window.setContentView(Some(&root));
            let theme = visible.then(theme::Theme::load);
            if let Some(theme) = &theme {
                theme.apply(&window, &web);
            }
            let state = Rc::new(RefCell::new(State {
                signature: super::file_signature(&args.path),
                args,
                web,
                resources,
                theme,
            }));
            let controls: Retained<Controls> = msg_send![
                super(Controls::alloc(mtm).set_ivars(Rc::clone(&state))),
                init
            ];
            if visible {
                let menu = NSMenu::new(mtm);
                let menu_root = NSMenuItem::new(mtm);
                let actions = NSMenu::new(mtm);
                let close = actions.addItemWithTitle_action_keyEquivalent(
                    &NSString::from_str("Close"),
                    Some(sel!(performClose:)),
                    &NSString::from_str("w"),
                );
                close.setTarget(Some(&window));
                let quit = actions.addItemWithTitle_action_keyEquivalent(
                    &NSString::from_str("Quit Flowmux Markdown"),
                    Some(sel!(terminate:)),
                    &NSString::from_str("q"),
                );
                quit.setTarget(Some(&app));
                menu_root.setSubmenu(Some(&actions));
                menu.addItem(&menu_root);
                app.setMainMenu(Some(&menu));
                for (tag, (title, key)) in [
                    ("Back", "["),
                    ("Forward", "]"),
                    ("Reload", "r"),
                    ("−", "-"),
                    ("100%", "0"),
                    ("+", "+"),
                ]
                .iter()
                .enumerate()
                {
                    let button = NSButton::buttonWithTitle_target_action(
                        &NSString::from_str(title),
                        Some(&controls),
                        Some(sel!(activate:)),
                        mtm,
                    );
                    button.setTag(tag as isize);
                    button.setFrame(NSRect::new(
                        NSPoint::new(8.0 + tag as f64 * 50.0, size.height - 34.0),
                        NSSize::new(48.0, 28.0),
                    ));
                    button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
                    button.setKeyEquivalent(&NSString::from_str(key));
                    button.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
                    root.addSubview(&button);
                }
                window.center();
                window.makeKeyAndOrderFront(None);
            } else {
                window.setAlphaValue(0.0);
                window.orderFront(None);
            }
            state.borrow().reload();
            Self {
                app,
                window,
                state,
                _controls: controls,
            }
        }
    }

    fn tick(&self) {
        autoreleasepool(|_| unsafe {
            if let Some(event) = self.app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&NSDate::dateWithTimeIntervalSinceNow(0.01)),
                NSDefaultRunLoopMode,
                true,
            ) {
                self.app.sendEvent(&event);
            }
            self.app.updateWindows();
        });
    }

    fn poll(&self) {
        let theme = theme::Theme::load();
        let changed = self
            .state
            .borrow()
            .theme
            .as_ref()
            .is_some_and(|old| old != &theme);
        if changed && !unsafe { self.state.borrow().web.isLoading() } {
            theme.apply(&self.window, &self.state.borrow().web);
            // Update styles in place so changing Flowmux's theme preserves scroll and zoom.
            let css = theme.css();
            if self.javascript(&format!("let style = document.getElementById('flowmux-theme'); if (!style) {{ style = document.createElement('style'); style.id = 'flowmux-theme'; document.head.append(style); }} style.textContent = {css:?}; return true;")).is_ok() {
                self.state.borrow_mut().theme = Some(theme);
            }
        }
        self.state.borrow_mut().poll();
    }

    fn wait<T>(&self, result: &RefCell<Option<Result<T, String>>>) -> Result<T, String> {
        let deadline = Instant::now() + Duration::from_secs(RENDER_TIMEOUT_SECS as u64);
        loop {
            if let Some(result) = result.borrow_mut().take() {
                return result;
            }
            if Instant::now() >= deadline {
                return Err("timed out rendering Markdown".into());
            }
            self.tick();
        }
    }

    fn javascript(&self, source: &str) -> Result<String, String> {
        let result = Rc::new(RefCell::new(None));
        let output = Rc::clone(&result);
        let callback = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| unsafe {
            *output.borrow_mut() = Some(if let Some(error) = error.as_ref() {
                Err(error.localizedDescription().to_string())
            } else if let Some(value) = value.as_ref() {
                let description: Retained<NSString> = msg_send![value, description];
                Ok(description.to_string())
            } else {
                Ok(String::new())
            });
        });
        unsafe {
            let world = WKContentWorld::worldWithName(
                &NSString::from_str(MARKDOWN_SCRIPT_WORLD),
                MainThreadMarker::new().unwrap(),
            );
            self.state
                .borrow()
                .web
                .callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                    &NSString::from_str(source),
                    None,
                    None,
                    &world,
                    Some(&callback),
                );
        }
        self.wait(&result)
    }

    fn ready(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(RENDER_TIMEOUT_SECS as u64);
        while unsafe { self.state.borrow().web.isLoading() } {
            if Instant::now() >= deadline {
                return Err("timed out loading Markdown".into());
            }
            self.tick();
        }
        self.javascript(super::WAIT_FOR_DIAGRAMS).map(|_| ())
    }

    fn save_png(&self, path: &Path) -> Result<(), String> {
        self.ready()?;
        self.javascript("await Promise.all([...document.images].map(i => i.decode().catch(() => {}))); return true;")?;
        let height: f64 = self
            .javascript("return document.documentElement.scrollHeight;")?
            .parse()
            .map_err(|_| "invalid document height")?;
        let zoom = self.state.borrow().args.options.normalized_zoom() as f64;
        let width = self.state.borrow().args.options.normalized_width() as f64;
        // AppKit constrains windows to the screen; snapshots need the entire WebView.
        self.state
            .borrow()
            .web
            .setFrameSize(NSSize::new(width, height * zoom));
        self.javascript("await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))); return true;")?;
        let result = Rc::new(RefCell::new(None));
        let output = Rc::clone(&result);
        let path = path.to_path_buf();
        let callback = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| unsafe {
            *output.borrow_mut() = Some(if let Some(error) = error.as_ref() {
                Err(error.localizedDescription().to_string())
            } else if let Some(image) = image.as_ref() {
                save_image(image, &path)
            } else {
                Err("WebKit returned no snapshot".into())
            });
        });
        unsafe {
            self.state
                .borrow()
                .web
                .takeSnapshotWithConfiguration_completionHandler(None, &callback);
        }
        self.wait(&result)
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        unsafe {
            self.state.borrow().web.stopLoading();
        }
        self.window.close();
    }
}

unsafe fn save_image(image: &NSImage, path: &Path) -> Result<(), String> {
    let tiff = image
        .TIFFRepresentation()
        .ok_or("snapshot has no image data")?;
    let bitmap = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &tiff)
        .ok_or("invalid snapshot image")?;
    let properties = NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new();
    let png = bitmap
        .representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
        .ok_or("encode PNG")?;
    if png.writeToFile_atomically(&NSString::from_str(&path.to_string_lossy()), true) {
        Ok(())
    } else {
        Err(format!("save {}: failed to write PNG", path.display()))
    }
}

pub(super) fn run_app(args: Args) {
    let viewer = Viewer::new(args, true);
    let mut polled = Instant::now();
    while viewer.window.isVisible() {
        viewer.tick();
        if polled.elapsed() >= Duration::from_millis(250) {
            viewer.poll();
            polled = Instant::now();
        }
    }
}

pub(super) fn render_png(input: &Path, output: &Path, options: &RenderOptions) -> ExitCode {
    // Fail before opening a window for unreadable input, as the Linux renderer does.
    if let Err(error) = render_markdown_file(input, options) {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    let viewer = Viewer::new(
        Args {
            path: input.to_path_buf(),
            options: options.clone(),
            window_height: 1,
            watch: false,
            render_png: None,
            help: false,
        },
        false,
    );
    match viewer.save_png(output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
