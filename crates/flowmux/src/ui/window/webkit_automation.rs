// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in WebKitWebDriver sessions. The driver owns these ephemeral windows.

use super::*;
use crate::ui::browser_pane::BrowserPane;
use glib::translate::*;
use webkit6::prelude::*;

impl WindowController {
    pub(crate) fn enable_webkit_automation(&self) {
        let host = Rc::new(AutomationHost {
            app: self
                .window
                .application()
                .expect("application window")
                .downgrade(),
            callbacks: self.callbacks.clone(),
            context: RefCell::new(None),
        });
        host.start();
        self.window
            .application()
            .unwrap()
            .connect_shutdown(move |_| {
                if let Some(context) = host.context.borrow_mut().take() {
                    context.set_automation_allowed(false);
                }
            });
        tracing::info!("WebKitWebDriver enabled for ephemeral Flowmux browser windows");
    }
}

struct AutomationHost {
    app: glib::WeakRef<gtk::Application>,
    callbacks: PaneCallbacks,
    context: RefCell<Option<webkit6::WebContext>>,
}

impl AutomationHost {
    fn start(self: &Rc<Self>) {
        let context = webkit6::WebContext::new();
        let host = Rc::downgrade(self);
        context.connect_automation_started(move |context, session| {
            let Some(host) = host.upgrade() else { return };
            let info = webkit6::ApplicationInfo::new();
            info.set_name("Flowmux");
            info.set_version(
                env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap(),
                env!("CARGO_PKG_VERSION_MINOR").parse().unwrap(),
                env!("CARGO_PKG_VERSION_PATCH").parse().unwrap(),
            );
            session.set_application_info(&info);
            let windows = Rc::new(AutomationWindows {
                app: host.app.clone(),
                context: context.clone(),
                callbacks: host.callbacks.clone(),
                windows: RefCell::new(HashMap::new()),
            });
            let create = windows.clone();
            // Each requested browsing context gets its own visible Flowmux window.
            session.connect_create_web_view(None, move |_| create.open(None));
            session.connect_local("will-close", false, move |_| {
                windows.close_all();
                let host = Rc::downgrade(&host);
                // WebKit reuses its automation data store; replace the context between sessions.
                glib::idle_add_local_once(move || {
                    if let Some(host) = host.upgrade() {
                        if let Some(context) = host.context.borrow_mut().take() {
                            context.set_automation_allowed(false);
                        }
                        host.start();
                    }
                });
                None
            });
        });
        context.set_automation_allowed(true);
        *self.context.borrow_mut() = Some(context);
    }
}

struct AutomationWindows {
    app: glib::WeakRef<gtk::Application>,
    context: webkit6::WebContext,
    callbacks: PaneCallbacks,
    windows: RefCell<HashMap<PaneId, (gtk::Window, BrowserPane)>>,
}

impl AutomationWindows {
    fn open(self: &Rc<Self>, related: Option<&webkit6::WebView>) -> webkit6::WebView {
        let builder = match related {
            Some(parent) => webkit6::WebView::builder().related_view(parent),
            None => webkit6::WebView::builder()
                .web_context(&self.context)
                .is_controlled_by_automation(true),
        };
        let view = builder.build();
        let id = PaneId::new();
        let mut callbacks = self.callbacks.clone();
        // These windows are owned by WebDriver, not the persisted pane registry.
        callbacks.on_browser_uri_changed = Rc::new(RefCell::new(|_, _, _| {}));
        callbacks.on_browser_title_changed = Rc::new(RefCell::new(|_, _, _| {}));
        let pane = BrowserPane::with_web_view(
            id,
            SurfaceId::new(),
            None,
            callbacks,
            view.clone(),
            flowmux_browser::BrowserProfile::Default,
        );
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&gtk::Label::new(Some("Flowmux · WebDriver"))));
        root.append(&header);
        root.append(&pane.root);
        let window = gtk::Window::builder()
            .title("Flowmux · WebDriver")
            .default_width(1000)
            .default_height(750)
            .child(&root)
            .build();
        if let Some(app) = self.app.upgrade() {
            window.set_application(Some(&app));
        }
        let weak_window = window.downgrade();
        view.connect_title_notify(move |view| {
            if let Some(window) = weak_window.upgrade() {
                let title = view.title().unwrap_or_default();
                window.set_title(Some(&format!("{title} — Flowmux · WebDriver")));
            }
        });
        connect_popup(&view, Rc::downgrade(self));
        let weak_view = view.downgrade();
        window.connect_close_request(move |_| {
            if let Some(view) = weak_view.upgrade() {
                view.try_close();
            }
            glib::Propagation::Stop
        });
        let owner = Rc::downgrade(self);
        view.connect_close(move |_| {
            // Dispose after WebKit's close signal has unwound.
            let owner = owner.clone();
            glib::idle_add_local_once(move || {
                if let Some(owner) = owner.upgrade() {
                    let closed = owner.windows.borrow_mut().remove(&id);
                    if let Some((window, _pane)) = closed {
                        window.set_child(None::<&gtk::Widget>);
                        window.destroy();
                    }
                }
            });
        });
        self.windows.borrow_mut().insert(id, (window.clone(), pane));
        if related.is_some() {
            let window = window.downgrade();
            view.connect_ready_to_show(move |_| {
                if let Some(window) = window.upgrade() {
                    window.present();
                }
            });
        } else {
            window.present();
        }
        view
    }

    fn close_all(&self) {
        let windows = std::mem::take(&mut *self.windows.borrow_mut());
        tracing::debug!(count = windows.len(), "closing WebDriver browser windows");
        for (_, (window, _pane)) in windows {
            window.set_child(None::<&gtk::Widget>);
            window.destroy();
        }
    }
}

fn connect_popup(view: &webkit6::WebView, owner: std::rc::Weak<AutomationWindows>) {
    unsafe extern "C" fn create(
        parent: *mut webkit6::ffi::WebKitWebView,
        _action: *mut webkit6::ffi::WebKitNavigationAction,
        data: glib::ffi::gpointer,
    ) -> *mut gtk::ffi::GtkWidget {
        let owner = &*(data as *const std::rc::Weak<AutomationWindows>);
        let Some(owner) = owner.upgrade() else {
            return std::ptr::null_mut();
        };
        let parent = from_glib_borrow(parent);
        // WebKit uses a parent-owned widget; the generated binding's extra ref leaks popups.
        owner.open(Some(&parent)).as_ptr().cast()
    }
    // The callback runs on GTK's thread; the session map retains each returned widget.
    unsafe {
        glib::signal::connect_raw(
            view.as_ptr().cast(),
            c"create".as_ptr(),
            Some(std::mem::transmute::<*const (), unsafe extern "C" fn()>(
                create as *const (),
            )),
            Box::into_raw(Box::new(owner)),
        );
    }
}
