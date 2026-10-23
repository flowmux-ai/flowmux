// SPDX-License-Identifier: GPL-3.0-or-later
//! macOS in-app browser pane backed by the system WebKit `WKWebView`.

use crate::ui::browser_bookmarks::BookmarkMenu;
use crate::ui::browser_downloads::{DownloadItem, DownloadManager};
use crate::ui::pane_terminal::PaneCallbacks;
use adw::prelude::*;
use flowmux_browser::{BrowserProfile, RefScope, RefStore};
use flowmux_config::options::BrowserEngine;
use flowmux_core::{PaneId, SurfaceId};
use gtk::glib::{self, translate::ToGlibPtr};
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, ProtocolObject};
use objc2::{
    define_class, msg_send, sel, AnyThread, ClassType, DefinedClass, MainThreadMarker,
    MainThreadOnly, Message,
};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSResponder, NSView, NSWindow};
use objc2_foundation::{
    NSDictionary, NSError, NSObjectProtocol, NSPoint, NSProgressReporting, NSRect, NSSize,
    NSString, NSURLRequest, NSURL, NSUUID,
};
use objc2_web_kit::{
    WKAudiovisualMediaTypes, WKContentWorld, WKDownload, WKDownloadDelegate, WKFrameInfo,
    WKMediaCaptureType, WKNavigation, WKNavigationAction, WKNavigationActionPolicy,
    WKNavigationDelegate, WKNavigationResponse, WKNavigationResponsePolicy, WKPermissionDecision,
    WKPreferences, WKSecurityOrigin, WKSnapshotConfiguration, WKUIDelegate, WKWebView,
    WKWebViewConfiguration, WKWebsiteDataStore, WKWindowFeatures,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

#[path = "browser_pane_macos/inspector.rs"]
mod inspector;

#[cfg(all(test, feature = "native-smoke"))]
#[path = "browser_pane_macos/inspector_smoke.rs"]
mod inspector_smoke;

#[cfg(all(test, feature = "native-smoke"))]
#[path = "browser_pane_macos/ssh_preview_smoke.rs"]
mod ssh_preview_smoke;

#[cfg(all(test, feature = "native-smoke"))]
#[path = "browser_pane_macos/stills_smoke.rs"]
pub(crate) mod stills_smoke;

thread_local! {
    static NATIVE_BROWSER_VIEW_SUSPEND_COUNT: Cell<u32> = const { Cell::new(0) };
    static NATIVE_VIEW_STILLS: RefCell<Vec<NativeViewStill>> = const { RefCell::new(Vec::new()) };
    // Suspended views stay visible until their placeholder shows a still.
    static NATIVE_VIEW_STILLS_PENDING: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

/// GTK placeholder that paints a snapshot of its WKWebView while suspended.
struct NativeViewStill {
    placeholder: glib::WeakRef<gtk::DrawingArea>,
    web_view: objc2::rc::Weak<WKWebView>,
    image: Rc<RefCell<Option<gtk::cairo::ImageSurface>>>,
}

unsafe extern "C" {
    fn gdk_macos_surface_get_native_window(surface: *mut gtk::gdk::ffi::GdkSurface) -> *mut c_void;
}

#[derive(Clone)]
pub struct BrowserPane {
    pane_id: Rc<Cell<PaneId>>,
    pub root: gtk::Box,
    pub web_view: gtk::Widget,
    native: Rc<NativeBrowserView>,
    address: gtk::Entry,
    zoom_label: gtk::Button,
    find_entry: gtk::SearchEntry,
    pub refs: Rc<RefCell<RefStore>>,
    pub ref_scope: RefScope,
}

struct NativeBrowserView {
    navigation_enabled: Rc<Cell<bool>>,
    web_view: Retained<WKWebView>,
    _ui_delegate: Retained<BrowserUIDelegate>,
    _navigation_delegate: Retained<BrowserNavigationDelegate>,
    last_url: RefCell<String>,
    last_title: RefCell<String>,
    zoom: Cell<f64>,
}

struct DownloadUi {
    item: DownloadItem,
    handle: Rc<RefCell<Option<Retained<WKDownload>>>>,
}

struct BrowserNavigationDelegateIvars {
    navigation_enabled: Rc<Cell<bool>>,
    refs: Rc<RefCell<RefStore>>,
    ref_scope: RefScope,
    download_manager: DownloadManager,
    downloads: RefCell<HashMap<usize, DownloadUi>>,
}

struct BrowserUIDelegateIvars {
    navigation_enabled: Rc<Cell<bool>>,
    pane_id: Rc<Cell<PaneId>>,
    open_url: Rc<RefCell<dyn FnMut(PaneId, String)>>,
    placeholder: glib::WeakRef<gtk::Widget>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The delegate is used
    // only on the main thread as required by WKUIDelegate.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = BrowserUIDelegateIvars]
    struct BrowserUIDelegate;

    unsafe impl NSObjectProtocol for BrowserUIDelegate {}

    unsafe impl WKUIDelegate for BrowserUIDelegate {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        #[allow(non_snake_case)]
        fn webView_createWebViewWithConfiguration_forNavigationAction_windowFeatures(
            &self,
            _web_view: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            navigation_action: &WKNavigationAction,
            _window_features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            let request = unsafe { navigation_action.request() };
            let url = request
                .URL()
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string());
            if let Some(url) = url.filter(|_| self.ivars().navigation_enabled.get()) {
                let ivars = self.ivars();
                (ivars.open_url.borrow_mut())(ivars.pane_id.get(), url);
            }
            None
        }

        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        #[allow(non_snake_case)]
        fn webView_requestMediaCapturePermissionForOrigin_initiatedByFrame_type_decisionHandler(
            &self,
            _web_view: &WKWebView,
            origin: &WKSecurityOrigin,
            _frame: &WKFrameInfo,
            capture_type: WKMediaCaptureType,
            decision_handler: &block2::DynBlock<dyn Fn(WKPermissionDecision)>,
        ) {
            let Some(parent) = self
                .ivars()
                .placeholder
                .upgrade()
                .and_then(|widget| widget.root())
                .and_then(|root| root.downcast::<gtk::Window>().ok())
            else {
                decision_handler.call((WKPermissionDecision::Deny,));
                return;
            };
            let site = unsafe { origin.host().to_string() };
            let site = if site.is_empty() { "This page" } else { &site };
            present_web_permission_dialog(
                &parent,
                media_capture_name(capture_type),
                site,
                decision_handler.copy(),
            );
        }

        // Without these panels WebKit silently cancels alert/confirm/prompt.
        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        #[allow(non_snake_case)]
        fn webView_runJavaScriptAlertPanelWithMessage_initiatedByFrame_completionHandler(
            &self,
            _web_view: &WKWebView,
            message: &NSString,
            frame: &WKFrameInfo,
            completion_handler: &block2::DynBlock<dyn Fn()>,
        ) {
            let handler = completion_handler.copy();
            present_page_dialog(
                self.ivars(),
                frame,
                message,
                PageDialog::Alert,
                PageDialogReply(Some(Box::new(move |_| handler.call(())))),
            );
        }

        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        #[allow(non_snake_case)]
        fn webView_runJavaScriptConfirmPanelWithMessage_initiatedByFrame_completionHandler(
            &self,
            _web_view: &WKWebView,
            message: &NSString,
            frame: &WKFrameInfo,
            completion_handler: &block2::DynBlock<dyn Fn(Bool)>,
        ) {
            let handler = completion_handler.copy();
            present_page_dialog(
                self.ivars(),
                frame,
                message,
                PageDialog::Confirm,
                PageDialogReply(Some(Box::new(move |reply| {
                    handler.call((Bool::new(reply.is_some()),))
                }))),
            );
        }

        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        #[allow(non_snake_case)]
        fn webView_runJavaScriptTextInputPanelWithPrompt_defaultText_initiatedByFrame_completionHandler(
            &self,
            _web_view: &WKWebView,
            prompt: &NSString,
            default_text: Option<&NSString>,
            frame: &WKFrameInfo,
            completion_handler: &block2::DynBlock<dyn Fn(*mut NSString)>,
        ) {
            let handler = completion_handler.copy();
            let default_text = default_text.map(NSString::to_string).unwrap_or_default();
            present_page_dialog(
                self.ivars(),
                frame,
                prompt,
                PageDialog::Prompt(default_text),
                PageDialogReply(Some(Box::new(move |reply| match reply {
                    Some(text) => {
                        let text = NSString::from_str(&text);
                        handler.call((Retained::as_ptr(&text).cast_mut(),));
                    }
                    None => handler.call((std::ptr::null_mut(),)),
                }))),
            );
        }
    }
);

define_class!(
    // SAFETY: NSObject has no subclassing requirements. WebKit invokes both
    // delegate protocols on the main thread.
    // Box the state because RefScope's u128 needs 16-byte alignment, while
    // objc2 only supports inline ivars with alignment up to 8 bytes.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Box<BrowserNavigationDelegateIvars>]
    struct BrowserNavigationDelegate;

    unsafe impl NSObjectProtocol for BrowserNavigationDelegate {}

    unsafe impl WKNavigationDelegate for BrowserNavigationDelegate {
        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        #[allow(non_snake_case)]
        fn webView_didStartProvisionalNavigation(
            &self,
            _web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
        ) {
            self.ivars().refs.borrow_mut().clear(self.ivars().ref_scope);
        }

        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        #[allow(non_snake_case)]
        fn webView_decidePolicyForNavigationAction_decisionHandler(
            &self,
            _web_view: &WKWebView,
            navigation_action: &WKNavigationAction,
            decision_handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let url = unsafe { navigation_action.request() }.URL()
                .and_then(|url| url.absoluteString()).map(|url| url.to_string());
            let policy = if !self.ivars().navigation_enabled.get() && url.as_deref() != Some("about:blank") {
                WKNavigationActionPolicy::Cancel
            } else if unsafe { navigation_action.shouldPerformDownload() } {
                WKNavigationActionPolicy::Download
            } else {
                WKNavigationActionPolicy::Allow
            };
            decision_handler.call((policy,));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        #[allow(non_snake_case)]
        fn webView_decidePolicyForNavigationResponse_decisionHandler(
            &self,
            _web_view: &WKWebView,
            navigation_response: &WKNavigationResponse,
            decision_handler: &block2::DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            let url = unsafe { navigation_response.response() }.URL()
                .and_then(|url| url.absoluteString()).map(|url| url.to_string());
            let policy = if !self.ivars().navigation_enabled.get() && url.as_deref() != Some("about:blank") {
                WKNavigationResponsePolicy::Cancel
            } else if unsafe { navigation_response.canShowMIMEType() } {
                WKNavigationResponsePolicy::Allow
            } else {
                WKNavigationResponsePolicy::Download
            };
            decision_handler.call((policy,));
        }

        #[unsafe(method(webView:navigationAction:didBecomeDownload:))]
        #[allow(non_snake_case)]
        fn webView_navigationAction_didBecomeDownload(
            &self,
            _web_view: &WKWebView,
            _navigation_action: &WKNavigationAction,
            download: &WKDownload,
        ) {
            self.begin_download(download);
        }

        #[unsafe(method(webView:navigationResponse:didBecomeDownload:))]
        #[allow(non_snake_case)]
        fn webView_navigationResponse_didBecomeDownload(
            &self,
            _web_view: &WKWebView,
            _navigation_response: &WKNavigationResponse,
            download: &WKDownload,
        ) {
            self.begin_download(download);
        }
    }

    unsafe impl WKDownloadDelegate for BrowserNavigationDelegate {
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        #[allow(non_snake_case)]
        fn download_decideDestinationUsingResponse_suggestedFilename_completionHandler(
            &self,
            download: &WKDownload,
            _response: &objc2_foundation::NSURLResponse,
            suggested_filename: &NSString,
            completion_handler: &block2::DynBlock<dyn Fn(*mut NSURL)>,
        ) {
            let key = download_key(download);
            let directory = &self.ivars().download_manager.directory();
            if let Err(error) = std::fs::create_dir_all(directory) {
                if let Some(ui) = self.ivars().downloads.borrow().get(&key) {
                    ui.item.fail(error.to_string());
                }
                completion_handler.call((std::ptr::null_mut(),));
                return;
            }

            let destination = available_download_path(directory, &suggested_filename.to_string());
            if let Some(ui) = self.ivars().downloads.borrow().get(&key) {
                ui.item.set_destination(&destination);
            }
            let path = NSString::from_str(&destination.to_string_lossy());
            let url = NSURL::fileURLWithPath(&path);
            completion_handler.call((Retained::as_ptr(&url).cast_mut(),));
        }

        #[unsafe(method(downloadDidFinish:))]
        #[allow(non_snake_case)]
        fn downloadDidFinish(&self, download: &WKDownload) {
            self.finish_download(download_key(download), None);
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        #[allow(non_snake_case)]
        fn download_didFailWithError_resumeData(
            &self,
            download: &WKDownload,
            error: &NSError,
            _resume_data: Option<&objc2_foundation::NSData>,
        ) {
            self.finish_download(download_key(download), Some(error.localizedDescription().to_string()));
        }
    }
);

impl BrowserNavigationDelegate {
    fn begin_download(&self, download: &WKDownload) {
        let key = download_key(download);
        if self.ivars().downloads.borrow().contains_key(&key) {
            return;
        }
        unsafe {
            download.setDelegate(Some(ProtocolObject::from_ref(self)));
        }

        let handle = Rc::new(RefCell::new(Some(download.retain())));
        let handle_for_cancel = handle.clone();
        let delegate = objc2::rc::Weak::new(self);
        let item = self.ivars().download_manager.add(move || {
            let download = handle_for_cancel.borrow().clone();
            if let Some(download) = download {
                let delegate = delegate.clone();
                // WKDownload cancellation completes here, not through its failure delegate.
                let completion = block2::RcBlock::new(move |_: *mut objc2_foundation::NSData| {
                    if let Some(delegate) = delegate.load() {
                        delegate.finish_download(key, None);
                    }
                });
                unsafe {
                    download.cancel(Some(&completion));
                }
            }
        });
        {
            let handle = handle.clone();
            let item = item.clone();
            glib::timeout_add_local(Duration::from_millis(100), move || {
                let Some(download) = handle.borrow().as_ref().cloned() else {
                    return glib::ControlFlow::Break;
                };
                item.set_progress(download.progress().fractionCompleted());
                glib::ControlFlow::Continue
            });
        }
        self.ivars()
            .downloads
            .borrow_mut()
            .insert(key, DownloadUi { item, handle });
    }

    fn finish_download(&self, key: usize, error: Option<String>) {
        let Some(ui) = self.ivars().downloads.borrow_mut().remove(&key) else {
            return;
        };
        ui.handle.borrow_mut().take();
        if let Some(error) = error {
            ui.item.fail(error);
        } else {
            ui.item.finish();
        }
    }
}

impl Drop for NativeBrowserView {
    fn drop(&mut self) {
        inspector::close(&self.web_view);
        let view = self.web_view.as_super();
        view.removeFromSuperview();
    }
}

pub struct NativeBrowserViewsSuspend {
    _views: Vec<Retained<NSView>>,
}

impl Drop for NativeBrowserViewsSuspend {
    fn drop(&mut self) {
        pop_native_browser_view_suspend();
        // The GTK pane may have been hidden or moved during the transition.
        // Its native frame synchronizer will reveal it only if still mapped;
        // restoring the old NSView flag can resurrect an inactive browser
        // over a zoomed terminal. This also handles nested suspensions.
    }
}

pub fn suspend_native_browser_views_for_window(window: &gtk::Window) -> NativeBrowserViewsSuspend {
    suspend_native_browser_views(window, true)
}

fn suspend_native_browser_views(
    window: &gtk::Window,
    capture_stills: bool,
) -> NativeBrowserViewsSuspend {
    push_native_browser_view_suspend();
    let Some(content_view) = native_content_view(window) else {
        return NativeBrowserViewsSuspend { _views: Vec::new() };
    };

    let mut views = Vec::new();
    collect_web_views(&content_view, &mut views);
    if capture_stills {
        freeze_visible_native_views(window);
    }
    for view in &views {
        if !capture_stills {
            NATIVE_VIEW_STILLS_PENDING
                .with_borrow_mut(|pending| pending.remove(&native_view_key(view)));
        }
        if !capture_stills || !native_view_still_pending(view) {
            view.setHidden(true);
        }
    }
    NativeBrowserViewsSuspend { _views: views }
}

/// Let suspension replace `web_view` with a still in `placeholder` instead of
/// blanking the pane while a dialog fades in over it.
pub(crate) fn install_native_view_still(placeholder: &gtk::DrawingArea, web_view: &WKWebView) {
    let image = Rc::new(RefCell::new(None::<gtk::cairo::ImageSurface>));
    let image_for_draw = image.clone();
    placeholder.set_draw_func(move |_, cr, width, height| {
        if !native_browser_views_are_suspended() {
            image_for_draw.borrow_mut().take();
            return;
        }
        let image = image_for_draw.borrow();
        let Some(image) = image.as_ref() else {
            return;
        };
        cr.scale(
            f64::from(width) / f64::from(image.width().max(1)),
            f64::from(height) / f64::from(image.height().max(1)),
        );
        if cr.set_source_surface(image, 0.0, 0.0).is_ok() {
            let _ = cr.paint();
        }
    });
    NATIVE_VIEW_STILLS.with_borrow_mut(|stills| {
        stills.retain(|still| still.placeholder.upgrade().is_some());
        stills.push(NativeViewStill {
            placeholder: placeholder.downgrade(),
            web_view: objc2::rc::Weak::new(web_view),
            image,
        });
    });
}

fn freeze_visible_native_views(window: &gtk::Window) {
    let targets = NATIVE_VIEW_STILLS.with_borrow(|stills| {
        stills
            .iter()
            .filter_map(|still| {
                let placeholder = still.placeholder.upgrade()?;
                let web_view = still.web_view.load()?;
                let visible = placeholder.is_mapped()
                    && placeholder.root().as_ref() == Some(window.upcast_ref::<gtk::Root>())
                    && !web_view.as_super().isHidden();
                visible.then(|| (placeholder, web_view, still.image.clone()))
            })
            .collect::<Vec<_>>()
    });
    for (placeholder, web_view, image) in targets {
        let key = native_view_key(web_view.as_super());
        NATIVE_VIEW_STILLS_PENDING.with_borrow_mut(|pending| pending.insert(key));
        let view = web_view.clone();
        let block = block2::RcBlock::new(
            move |snapshot: *mut objc2_app_kit::NSImage, _error: *mut NSError| {
                NATIVE_VIEW_STILLS_PENDING.with_borrow_mut(|pending| pending.remove(&key));
                if !native_browser_views_are_suspended() {
                    return;
                }
                if let Some(snapshot) = unsafe { snapshot.as_ref() } {
                    image.replace(cairo_surface_from_nsimage(snapshot));
                    placeholder.queue_draw();
                }
                view.as_super().setHidden(true);
            },
        );
        unsafe {
            web_view.takeSnapshotWithConfiguration_completionHandler(
                Option::<&WKSnapshotConfiguration>::None,
                &block,
            );
        }
    }
}

fn cairo_surface_from_nsimage(image: &objc2_app_kit::NSImage) -> Option<gtk::cairo::ImageSurface> {
    let tiff = image.TIFFRepresentation()?;
    let texture = gtk::gdk::Texture::from_bytes(&glib::Bytes::from_owned(tiff.to_vec())).ok()?;
    let mut surface = gtk::cairo::ImageSurface::create(
        gtk::cairo::Format::ARgb32,
        texture.width(),
        texture.height(),
    )
    .ok()?;
    let stride = surface.stride() as usize;
    // GDK's default download format matches Cairo's premultiplied ARGB32.
    texture.download(&mut surface.data().ok()?, stride);
    Some(surface)
}

fn native_view_key(view: &NSView) -> usize {
    view as *const NSView as usize
}

fn native_view_still_pending(view: &NSView) -> bool {
    NATIVE_VIEW_STILLS_PENDING.with_borrow(|pending| pending.contains(&native_view_key(view)))
}

/// A suspended view is hidden once its still is ready, or at once without one.
pub(crate) fn native_view_is_suspended(view: &NSView) -> bool {
    native_browser_views_are_suspended() && !native_view_still_pending(view)
}

fn push_native_browser_view_suspend() {
    NATIVE_BROWSER_VIEW_SUSPEND_COUNT.with(|count| {
        count.set(count.get().saturating_add(1));
    });
}

fn pop_native_browser_view_suspend() {
    NATIVE_BROWSER_VIEW_SUSPEND_COUNT.with(|count| {
        let next = count.get().saturating_sub(1);
        count.set(next);
        if next == 0 {
            NATIVE_VIEW_STILLS.with_borrow(|stills| {
                for still in stills {
                    still.image.borrow_mut().take();
                    if let Some(placeholder) = still.placeholder.upgrade() {
                        placeholder.queue_draw();
                    }
                }
            });
        }
    })
}

pub(crate) fn native_browser_views_are_suspended() -> bool {
    NATIVE_BROWSER_VIEW_SUSPEND_COUNT.with(|count| count.get() > 0)
}

fn collect_web_views(view: &NSView, views: &mut Vec<Retained<NSView>>) {
    if view.isKindOfClass(WKWebView::class()) {
        views.push(view.retain());
    }

    let subviews = view.subviews();
    for index in 0..subviews.count() {
        let subview = subviews.objectAtIndex(index);
        collect_web_views(&subview, views);
    }
}

/// Build a [`RefScope`] from a [`SurfaceId`]. The scope is just the
/// surface uuid as u128 — opaque to the store, stable across calls.
pub fn ref_scope_for_surface(surface_id: SurfaceId) -> RefScope {
    RefScope::from_u128(surface_id.0.as_u128())
}

impl BrowserPane {
    pub fn download(
        &self,
        action: &flowmux_ipc::protocol::BrowserDownloadAction,
    ) -> Result<String, String> {
        self.native
            ._navigation_delegate
            .ivars()
            .download_manager
            .control(action)
    }

    pub fn new(
        id: PaneId,
        surface_id: SurfaceId,
        initial_url: Option<&str>,
        callbacks: PaneCallbacks,
        engine: BrowserEngine,
        persist_session: bool,
    ) -> Self {
        let pane_id = Rc::new(Cell::new(id));
        let profile = engine_to_profile(&engine);
        tracing::debug!(
            engine = ?engine,
            profile = ?profile,
            persist_session,
            "creating browser pane (macOS WKWebView + profile-isolated WebsiteDataStore)"
        );

        let mtm = MainThreadMarker::new().expect("WKWebView must be created on the main thread");
        let web_view = create_web_view(mtm, &profile, persist_session);

        let back = gtk::Button::from_icon_name("go-previous-symbolic");
        let forward = gtk::Button::from_icon_name("go-next-symbolic");
        let reload = gtk::Button::from_icon_name("view-refresh-symbolic");
        let find = gtk::Button::from_icon_name("edit-find-symbolic");
        find.set_tooltip_text(Some("Find in page"));
        let zoom_out = gtk::Button::from_icon_name("zoom-out-symbolic");
        zoom_out.set_tooltip_text(Some("Zoom out"));
        let zoom_reset = gtk::Button::with_label("100%");
        zoom_reset.set_tooltip_text(Some("Reset page zoom"));
        let zoom_in = gtk::Button::from_icon_name("zoom-in-symbolic");
        zoom_in.set_tooltip_text(Some("Zoom in"));
        let zoom_label = zoom_reset.clone();
        let address = gtk::Entry::new();
        address.set_hexpand(true);
        address.set_placeholder_text(Some("Enter URL — e.g. http://localhost:3000"));
        let inspector = gtk::Button::from_icon_name("applications-utilities-symbolic");
        inspector.set_widget_name("flowmux-web-inspector");
        inspector.add_css_class("flat");
        inspector.set_tooltip_text(Some("Open Web Inspector"));
        let downloads = DownloadManager::new();
        let bookmarks = BookmarkMenu::new(
            &profile,
            {
                let web_view = web_view.clone();
                Rc::new(move || {
                    let url = current_url_native(&web_view);
                    if url.is_empty() {
                        return None;
                    }
                    let title = current_title_native(&web_view);
                    Some(flowmux_browser::Bookmark {
                        title: if title.trim().is_empty() {
                            url.clone()
                        } else {
                            title
                        },
                        url,
                    })
                })
            },
            {
                let web_view = web_view.clone();
                Rc::new(move |url| load_uri_native(&web_view, url))
            },
        );
        bookmarks.button.set_tooltip_text(Some(&format!(
            "Bookmarks\nProfile: {}\n{}",
            profile.display_name(),
            if persist_session {
                "Cookies and site data are saved in this browser profile"
            } else {
                "Cookies and site data are discarded when flowmux exits"
            }
        )));

        let chrome = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        chrome.set_margin_top(4);
        chrome.set_margin_bottom(4);
        chrome.set_margin_start(4);
        chrome.set_margin_end(4);
        chrome.append(&back);
        chrome.append(&forward);
        chrome.append(&reload);
        chrome.append(&address);
        chrome.append(&bookmarks.button);
        chrome.append(&find);
        chrome.append(&zoom_out);
        chrome.append(&zoom_reset);
        chrome.append(&zoom_in);
        chrome.append(&downloads.button());
        chrome.append(&inspector);

        let find_entry = gtk::SearchEntry::builder()
            .placeholder_text("Find in page…")
            .visible(false)
            .build();
        find_entry.set_margin_start(4);
        find_entry.set_margin_end(4);
        find_entry.set_margin_bottom(4);

        let viewport = gtk::DrawingArea::new();
        viewport.set_hexpand(true);
        viewport.set_vexpand(true);
        viewport.set_focusable(true);
        install_native_view_still(&viewport, &web_view);
        let web_widget = viewport.clone().upcast::<gtk::Widget>();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.append(&chrome);
        root.append(&find_entry);
        root.append(&viewport);

        let refs = Rc::new(RefCell::new(RefStore::new()));
        let ref_scope = ref_scope_for_surface(surface_id);
        let navigation_enabled = Rc::new(Cell::new(true));
        let native = Rc::new(NativeBrowserView {
            navigation_enabled: navigation_enabled.clone(),
            web_view: web_view.clone(),
            _ui_delegate: install_ui_delegate(
                mtm,
                &web_view,
                pane_id.clone(),
                callbacks.on_open_url.clone(),
                web_widget.downgrade(),
                navigation_enabled.clone(),
            ),
            _navigation_delegate: install_navigation_delegate(
                mtm,
                &web_view,
                downloads,
                refs.clone(),
                ref_scope,
                navigation_enabled,
            ),
            last_url: RefCell::new(String::new()),
            last_title: RefCell::new(String::new()),
            zoom: Cell::new(1.0),
        });

        {
            let native = native.clone();
            back.connect_clicked(move |_| {
                if unsafe { native.web_view.canGoBack() } {
                    unsafe {
                        native.web_view.goBack();
                    }
                }
            });
        }
        {
            let native = native.clone();
            forward.connect_clicked(move |_| {
                if unsafe { native.web_view.canGoForward() } {
                    unsafe {
                        native.web_view.goForward();
                    }
                }
            });
        }
        {
            let native = native.clone();
            reload.connect_clicked(move |_| unsafe {
                native.web_view.reload();
            });
        }
        {
            let entry = find_entry.clone();
            find.connect_clicked(move |_| {
                let visible = !entry.is_visible();
                entry.set_visible(visible);
                if visible {
                    entry.grab_focus();
                }
            });
        }
        {
            let native = native.clone();
            let label = zoom_label.clone();
            zoom_out.connect_clicked(move |_| {
                set_native_zoom(&native, &label, native.zoom.get() - 0.1);
            });
        }
        {
            let native = native.clone();
            zoom_reset.connect_clicked(move |label| {
                set_native_zoom(&native, label, 1.0);
            });
        }
        {
            let native = native.clone();
            let label = zoom_label.clone();
            zoom_in.connect_clicked(move |_| {
                set_native_zoom(&native, &label, native.zoom.get() + 0.1);
            });
        }
        {
            let native = native.clone();
            find_entry.connect_search_changed(move |entry| {
                find_in_native_page(&native.web_view, &entry.text());
            });
        }
        {
            let native = native.clone();
            find_entry.connect_activate(move |entry| {
                find_in_native_page(&native.web_view, &entry.text());
            });
        }
        {
            let native = native.clone();
            find_entry.connect_stop_search(move |entry| {
                entry.set_visible(false);
                focus_native_view(&native.web_view);
            });
        }
        {
            let native = native.clone();
            inspector.connect_clicked(move |button| {
                if let Err(message) = inspector::show(&native.web_view) {
                    let dialog = adw::AlertDialog::new(Some("Web Inspector"), Some(message));
                    dialog.add_response("close", "Close");
                    dialog.present(Some(button));
                }
            });
        }
        {
            let native = native.clone();
            address.connect_activate(move |address| {
                let uri = normalize_uri(&address.text());
                load_uri_native(&native.web_view, &uri);
            });
        }

        {
            let native = native.clone();
            let web_widget = web_widget.clone();
            root.connect_map(move |_| sync_native_view_frame(&native.web_view, &web_widget));
        }
        {
            let native = native.clone();
            root.connect_unmap(move |_| native.web_view.as_super().setHidden(true));
        }
        {
            let native = native.clone();
            root.connect_destroy(move |_| native.web_view.as_super().removeFromSuperview());
        }
        {
            // WKWebView is a native sibling of the GTK hierarchy, so page
            // clicks never reach BrowserPane.root's GTK focus controller.
            let native = Rc::downgrade(&native);
            let web_widget = web_widget.downgrade();
            let pane_id = pane_id.clone();
            let on_focus = callbacks.on_focus.clone();
            let native_focused = Cell::new(false);
            gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
                let Some(native) = native.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let Some(web_widget) = web_widget.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if !web_widget.is_mapped() {
                    native_focused.set(false);
                    return glib::ControlFlow::Continue;
                }
                sync_native_view_frame(&native.web_view, &web_widget);
                let focused = native_view_has_focus(&native.web_view);
                if native_focus_entered(&native_focused, focused) {
                    (on_focus.borrow_mut())(pane_id.get());
                }
                glib::ControlFlow::Continue
            });
        }
        {
            let native = Rc::downgrade(&native);
            let address = address.downgrade();
            let uri_cb = callbacks.on_browser_uri_changed.clone();
            let title_cb = callbacks.on_browser_title_changed.clone();
            let pane_id = pane_id.clone();
            gtk::glib::timeout_add_local(Duration::from_millis(250), move || {
                let Some(native) = native.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let Some(address) = address.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                sync_browser_state(
                    &native,
                    &address,
                    pane_id.get(),
                    surface_id,
                    &uri_cb,
                    &title_cb,
                );
                glib::ControlFlow::Continue
            });
        }

        let initial = initial_url
            .map(normalize_uri)
            .unwrap_or_else(|| "about:blank".into());
        address.set_text(&initial);
        load_uri_native(&native.web_view, &initial);

        Self {
            pane_id,
            root,
            web_view: web_widget,
            native,
            address,
            zoom_label,
            find_entry,
            refs,
            ref_scope,
        }
    }

    /// Call on the GTK main thread after isolating XDG state and registering the app.
    #[cfg(all(test, feature = "native-smoke"))]
    pub(crate) async fn check_native_lifetime(app: &gtk::Application) {
        async fn wait_for(description: &str, mut ready: impl FnMut() -> bool) {
            glib::future_with_timeout(Duration::from_secs(30), async {
                while !autoreleasepool(|_| ready()) {
                    glib::timeout_future(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("browser lifetime timed out: {description}"));
        }

        async fn javascript(browser: &BrowserPane, script: &str) -> String {
            let (tx, rx) = tokio::sync::oneshot::channel();
            browser.evaluate_js(script, move |result| {
                let _ = tx.send(result);
            });
            glib::future_with_timeout(Duration::from_secs(10), rx)
                .await
                .expect("browser lifetime JavaScript timed out")
                .unwrap()
                .unwrap()
        }

        let fixture = tempfile::Builder::new()
            .prefix("fm-b01-")
            .tempdir_in("/tmp")
            .unwrap();
        let page = fixture.path().join("page.html");
        std::fs::write(
            &page,
            "<!doctype html><title>B01 loaded</title><body>find this text</body>",
        )
        .unwrap();
        let url = format!("file://{}", page.display());
        let callbacks = PaneCallbacks::noop_for_test();
        // These callbacks are held only by the 50 ms and 250 ms timer closures.
        let focus_timer = Rc::downgrade(&callbacks.on_focus);
        let state_timer = Rc::downgrade(&callbacks.on_browser_title_changed);
        let closed = autoreleasepool(|_| {
            Self::new(
                PaneId::new(),
                SurfaceId::new(),
                None,
                callbacks,
                BrowserEngine::Webkit,
                false,
            )
        });
        let updates = Rc::new(Cell::new(0));
        let mut callbacks = PaneCallbacks::noop_for_test();
        let updates_for_callback = updates.clone();
        callbacks.on_browser_title_changed = Rc::new(RefCell::new(move |_, _, _| {
            updates_for_callback.set(updates_for_callback.get() + 1);
        }));
        let survivor = autoreleasepool(|_| {
            Self::new(
                PaneId::new(),
                SurfaceId::new(),
                Some(&url),
                callbacks,
                BrowserEngine::Webkit,
                false,
            )
        });
        let panes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        panes.append(&closed.root);
        panes.append(&survivor.root);
        let window = gtk::Window::builder()
            .application(app)
            .default_width(1000)
            .default_height(600)
            .child(&panes)
            .build();
        window.present();
        wait_for("both native browsers mapped", || {
            closed.web_view.is_mapped()
                && survivor.web_view.is_mapped()
                && closed.native.web_view.as_super().window().is_some()
                && survivor.native.web_view.as_super().window().is_some()
        })
        .await;
        closed.address.set_text(&url);
        closed.address.emit_activate();
        wait_for("address activation loaded the page", || {
            closed.current_title() == "B01 loaded"
        })
        .await;
        closed.set_zoom_level(1.5);
        closed.zoom_label.emit_clicked();
        assert_eq!(closed.native.zoom.get(), 1.0);
        assert_eq!(closed.zoom_label.label().as_deref(), Some("100%"));
        assert_eq!(unsafe { closed.native.web_view.pageZoom() }, 1.0);
        closed.show_search();
        closed.find_entry.set_text("find this text");
        closed.find_entry.emit_activate();
        assert_eq!(
            javascript(&closed, "window.getSelection().toString()").await,
            "find this text"
        );
        closed.find_entry.emit_stop_search();
        assert!(!closed.find_entry.is_visible());
        wait_for("survivor state timer active", || updates.get() > 0).await;
        assert!(focus_timer.upgrade().is_some());
        assert!(state_timer.upgrade().is_some());

        let widgets = [
            closed.root.clone().upcast::<gtk::Widget>().downgrade(),
            closed.web_view.downgrade(),
            closed.address.clone().upcast::<gtk::Widget>().downgrade(),
            closed
                .zoom_label
                .clone()
                .upcast::<gtk::Widget>()
                .downgrade(),
            closed
                .find_entry
                .clone()
                .upcast::<gtk::Widget>()
                .downgrade(),
        ];
        let native = Rc::downgrade(&closed.native);
        let web_view = objc2::rc::Weak::from_retained(&closed.native.web_view);
        autoreleasepool(|_| {
            closed.prepare_for_close();
            panes.remove(&closed.root);
            drop(closed);
        });
        wait_for("GTK widgets and native wrapper released", || {
            widgets.iter().all(|widget| widget.upgrade().is_none()) && native.upgrade().is_none()
        })
        .await;
        wait_for("WKWebView released", || web_view.load().is_none()).await;
        wait_for("both timer closures released", || {
            focus_timer.upgrade().is_none() && state_timer.upgrade().is_none()
        })
        .await;
        let previous_updates = updates.get();
        assert_eq!(
            javascript(&survivor, "document.title = 'B01 survivor'; 6 * 7").await,
            "42"
        );
        wait_for("survivor state timer still active", || {
            updates.get() > previous_updates
        })
        .await;
        assert_eq!(survivor.current_title(), "B01 survivor");
        autoreleasepool(|_| {
            survivor.prepare_for_close();
            panes.remove(&survivor.root);
            drop(survivor);
        });
        window.destroy();
        println!("MACOS_NATIVE_BROWSER_LIFETIME_OK");
    }

    pub fn current_url(&self) -> String {
        current_url_native(&self.native.web_view)
    }

    pub fn current_title(&self) -> String {
        current_title_native(&self.native.web_view)
    }

    pub fn load_uri(&self, url: &str) {
        if !self.native.navigation_enabled.get() {
            return;
        }
        let normalized = normalize_uri(url);
        self.address.set_text(&normalized);
        load_uri_native(&self.native.web_view, &normalized);
    }

    pub fn go_back(&self) -> bool {
        let moved =
            self.native.navigation_enabled.get() && unsafe { self.native.web_view.canGoBack() };
        if moved {
            unsafe {
                self.native.web_view.goBack();
            }
        }
        moved
    }

    pub fn go_forward(&self) -> bool {
        let moved =
            self.native.navigation_enabled.get() && unsafe { self.native.web_view.canGoForward() };
        if moved {
            unsafe {
                self.native.web_view.goForward();
            }
        }
        moved
    }

    pub fn reload(&self) {
        if !self.native.navigation_enabled.get() {
            return;
        }
        unsafe {
            self.native.web_view.reload();
        }
    }

    pub fn stop_loading(&self) {
        unsafe {
            self.native.web_view.stopLoading();
        }
    }

    pub fn expire_ssh_preview(&self) {
        if !self.native.navigation_enabled.replace(false) {
            return;
        }
        self.stop_loading();
        self.refs.borrow_mut().clear(self.ref_scope);
        // Disable scripts in the retiring document before releasing its forward.
        #[allow(deprecated)]
        unsafe {
            self.native
                .web_view
                .configuration()
                .preferences()
                .setJavaScriptEnabled(false);
        }
        load_uri_native(&self.native.web_view, "about:blank");
        self.root.set_sensitive(false);
    }

    pub fn prepare_for_close(&self) {
        inspector::close(&self.native.web_view);
        self.stop_loading();
    }

    pub fn grab_focus(&self) {
        focus_native_view(&self.native.web_view);
    }

    pub fn show_search(&self) {
        self.find_entry.set_visible(true);
        self.find_entry.grab_focus();
        self.find_entry.select_region(0, -1);
    }

    pub fn pane_id_handle(&self) -> Rc<Cell<PaneId>> {
        self.pane_id.clone()
    }

    pub fn set_pane_id(&self, id: PaneId) {
        self.pane_id.set(id);
    }

    pub fn focus_widget(&self) -> gtk::Widget {
        self.web_view.clone()
    }

    pub fn set_zoom_level(&self, zoom: f64) {
        set_native_zoom(&self.native, &self.zoom_label, zoom);
    }

    pub fn snapshot_to_png<F: FnOnce(Result<String, String>) + 'static>(
        &self,
        path: PathBuf,
        document: Option<flowmux_browser::snapshot::CaptureSize>,
        on_done: F,
    ) {
        let configuration = if let Some(size) = document {
            let scale = self
                .native
                .web_view
                .as_super()
                .window()
                .map_or(1.0, |window| unsafe {
                    msg_send![&*window, backingScaleFactor]
                });
            if let Err(error) = size.check(scale * unsafe { self.native.web_view.pageZoom() }) {
                on_done(Err(error));
                return;
            }
            let config = unsafe { WKSnapshotConfiguration::new(MainThreadMarker::new().unwrap()) };
            let supported: Bool =
                unsafe { msg_send![&*config, respondsToSelector: sel!(_setUsesContentsRect:)] };
            if !supported.as_bool() {
                // shortcut: older WebKit needs a separately validated tiled-capture fallback.
                on_done(Err(
                    "full-document capture requires macOS 15 or newer WebKit".into(),
                ));
                return;
            }
            // The content-rect snapshot paints offscreen content without resizing or scrolling.
            unsafe {
                let _: () = msg_send![&*config, _setUsesContentsRect: true];
            }
            Some(config)
        } else {
            None
        };
        let callback = Rc::new(RefCell::new(Some(on_done)));
        let path_for_block = path.clone();
        let block = block2::RcBlock::new(
            move |image: *mut objc2_app_kit::NSImage, error: *mut NSError| {
                let result = if !error.is_null() {
                    Err(error_description(error))
                } else if image.is_null() {
                    Err("WKWebView snapshot returned no image".into())
                } else {
                    unsafe { save_nsimage_to_png(&*image, &path_for_block, document) }
                };
                if let Some(callback) = callback.borrow_mut().take() {
                    callback(result);
                }
            },
        );
        unsafe {
            self.native
                .web_view
                .takeSnapshotWithConfiguration_completionHandler(configuration.as_deref(), &block);
        }
    }

    /// Run JS and call `on_done` with the JS result string. The
    /// scriptable API wraps this with a oneshot channel that the IPC
    /// handler awaits.
    pub fn evaluate_js<F: FnOnce(Result<String, String>) + 'static>(
        &self,
        source: &str,
        on_done: F,
    ) {
        self.evaluate_in_world(source, false, false, on_done);
    }

    pub fn evaluate_automation_js<F: FnOnce(Result<String, String>) + 'static>(
        &self,
        source: &str,
        on_done: F,
    ) {
        self.evaluate_in_world(source, true, false, on_done);
    }

    pub fn evaluate_async_js<F: FnOnce(Result<String, String>) + 'static>(
        &self,
        body: &str,
        on_done: F,
    ) {
        self.evaluate_in_world(body, false, true, on_done);
    }

    pub fn evaluate_automation_async_js<F: FnOnce(Result<String, String>) + 'static>(
        &self,
        body: &str,
        on_done: F,
    ) {
        self.evaluate_in_world(body, true, true, on_done);
    }

    pub fn insert_text(&self, text: &str) -> Result<(), String> {
        if !self.native.navigation_enabled.get() {
            return Err("SSH preview expired; open a new preview after reconnecting".into());
        }
        if !self.web_view.is_mapped() || !self.web_view.grab_focus() {
            return Err("native text input requires a visible browser tab".into());
        }
        focus_native_view(&self.native.web_view);
        crate::macos_input::insert_text(&self.native.web_view, text)
    }

    pub fn press_key(&self, stroke: &flowmux_browser::input::KeyStroke) -> Result<(), String> {
        if !self.native.navigation_enabled.get()
            || !self.web_view.is_mapped()
            || !self.web_view.grab_focus()
        {
            return Err("native key input requires a visible, active browser tab".into());
        }
        focus_native_view(&self.native.web_view);
        crate::macos_input::press_key(&self.native.web_view, stroke)
    }

    pub fn finish_key_input(
        &self,
        stroke: &flowmux_browser::input::KeyStroke,
    ) -> Result<(), String> {
        // GTK has no AppKit Select All menu item to handle an unconsumed Cmd+A.
        if stroke.meta && !stroke.control && !stroke.alt && !stroke.shift && stroke.key == "a" {
            let web = &self.native.web_view;
            let responder = web
                .as_super()
                .window()
                .and_then(|window| window.firstResponder())
                .filter(|responder| {
                    responder
                        .downcast_ref::<NSView>()
                        .is_some_and(|view| view.isDescendantOf(web.as_super()))
                })
                .ok_or("browser lost keyboard focus")?;
            if !unsafe { responder.tryToPerform_with(objc2::sel!(selectAll:), None) } {
                return Err("focused browser cannot select all".into());
            }
        }
        Ok(())
    }

    pub fn pointer(
        &self,
        action: flowmux_browser::input::PointerAction,
        target: &flowmux_browser::input::PointerTarget,
    ) -> Result<(), String> {
        if !self.native.navigation_enabled.get() || !self.web_view.is_mapped() {
            return Err("native pointer input requires a visible browser tab".into());
        }
        let bounds = self.native.web_view.as_super().bounds();
        let (x, y) = target.scale(bounds.size.width, bounds.size.height)?;
        crate::macos_input::pointer(&self.native.web_view, action, NSPoint::new(x, y))
    }

    fn evaluate_in_world<F: FnOnce(Result<String, String>) + 'static>(
        &self,
        source: &str,
        isolated: bool,
        asynchronous: bool,
        on_done: F,
    ) {
        if !self.native.navigation_enabled.get() {
            on_done(Err(
                "SSH preview expired; open a new preview after reconnecting".into(),
            ));
            return;
        }
        let script = NSString::from_str(source);
        let navigation_enabled = self.native.navigation_enabled.clone();
        let callback = Rc::new(RefCell::new(Some(on_done)));
        let block = block2::RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let result = if !navigation_enabled.get() {
                Err("SSH preview expired; open a new preview after reconnecting".into())
            } else if !error.is_null() {
                Err(error_description(error))
            } else {
                Ok(object_description(value))
            };
            if let Some(callback) = callback.borrow_mut().take() {
                callback(result);
            }
        });
        unsafe {
            if asynchronous {
                let mtm = MainThreadMarker::new().unwrap();
                let world = if isolated {
                    WKContentWorld::defaultClientWorld(mtm)
                } else {
                    WKContentWorld::pageWorld(mtm)
                };
                self.native
                    .web_view
                    .callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                        &script,
                        None,
                        None,
                        &world,
                        Some(&block),
                    );
            } else if isolated {
                let world = WKContentWorld::defaultClientWorld(MainThreadMarker::new().unwrap());
                self.native
                    .web_view
                    .evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                        &script,
                        None,
                        &world,
                        Some(&block),
                    );
            } else {
                self.native
                    .web_view
                    .evaluateJavaScript_completionHandler(&script, Some(&block));
            }
        }
    }
}

fn find_in_native_page(web_view: &WKWebView, query: &str) {
    if query.is_empty() {
        return;
    }
    let query = serde_json::to_string(query).unwrap_or_else(|_| "\"\"".into());
    let script = NSString::from_str(&format!(
        "window.find({query}, false, false, true, false, false, false)"
    ));
    unsafe {
        web_view.evaluateJavaScript_completionHandler(&script, None);
    }
}

fn create_web_view(
    mtm: MainThreadMarker,
    profile: &BrowserProfile,
    persist_session: bool,
) -> Retained<WKWebView> {
    let config = unsafe { WKWebViewConfiguration::new(mtm) };
    let preferences = unsafe { WKPreferences::new(mtm) };
    inspector::configure(&preferences);
    unsafe {
        preferences.setJavaScriptCanOpenWindowsAutomatically(true);
        preferences.setTabFocusesLinks(true);
        preferences.setElementFullscreenEnabled(true);
        config.setPreferences(&preferences);
        config.setMediaTypesRequiringUserActionForPlayback(WKAudiovisualMediaTypes::None);
        config.setWebsiteDataStore(&website_data_store(mtm, profile, persist_session));
    }

    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0));
    let web_view =
        unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config) };
    unsafe {
        web_view.setInspectable(true);
        web_view.setAllowsBackForwardNavigationGestures(true);
        web_view.setPageZoom(1.0);
    }
    web_view
}

fn install_ui_delegate(
    mtm: MainThreadMarker,
    web_view: &WKWebView,
    pane_id: Rc<Cell<PaneId>>,
    open_url: Rc<RefCell<dyn FnMut(PaneId, String)>>,
    placeholder: glib::WeakRef<gtk::Widget>,
    navigation_enabled: Rc<Cell<bool>>,
) -> Retained<BrowserUIDelegate> {
    let delegate = BrowserUIDelegate::alloc(mtm).set_ivars(BrowserUIDelegateIvars {
        navigation_enabled,
        pane_id,
        open_url,
        placeholder,
    });
    let delegate: Retained<BrowserUIDelegate> = unsafe { msg_send![super(delegate), init] };
    unsafe {
        web_view.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    }
    delegate
}

fn install_navigation_delegate(
    mtm: MainThreadMarker,
    web_view: &WKWebView,
    download_manager: DownloadManager,
    refs: Rc<RefCell<RefStore>>,
    ref_scope: RefScope,
    navigation_enabled: Rc<Cell<bool>>,
) -> Retained<BrowserNavigationDelegate> {
    let delegate =
        BrowserNavigationDelegate::alloc(mtm).set_ivars(Box::new(BrowserNavigationDelegateIvars {
            navigation_enabled,
            refs,
            ref_scope,
            download_manager,
            downloads: RefCell::new(HashMap::new()),
        }));
    let delegate: Retained<BrowserNavigationDelegate> = unsafe { msg_send![super(delegate), init] };
    unsafe {
        web_view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    }
    delegate
}

fn download_key(download: &WKDownload) -> usize {
    std::ptr::from_ref(download).addr()
}

fn available_download_path(directory: &Path, suggested: &str) -> PathBuf {
    let file_name = Path::new(suggested)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("download");
    let candidate = directory.join(file_name);
    if !candidate.exists() {
        return candidate;
    }

    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = path.extension().and_then(|value| value.to_str());
    for number in 1.. {
        let numbered = match extension {
            Some(extension) => format!("{stem} ({number}).{extension}"),
            None => format!("{stem} ({number})"),
        };
        let candidate = directory.join(numbered);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

fn media_capture_name(capture_type: WKMediaCaptureType) -> &'static str {
    match capture_type {
        WKMediaCaptureType::Camera => "camera access",
        WKMediaCaptureType::Microphone => "microphone access",
        WKMediaCaptureType::CameraAndMicrophone => "camera and microphone access",
        _ => "media capture access",
    }
}

fn present_web_permission_dialog(
    parent: &gtk::Window,
    permission: &str,
    site: &str,
    decision_handler: block2::RcBlock<dyn Fn(WKPermissionDecision)>,
) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Allow {permission}?")),
        Some(&format!("{site} is requesting access.")),
    );
    dialog.add_response("deny", "Deny");
    dialog.add_response("allow", "Allow");
    dialog.set_default_response(Some("deny"));
    dialog.set_close_response("deny");
    dialog.set_response_appearance("allow", adw::ResponseAppearance::Suggested);

    // Do not wait for WebKit to paint while it awaits a native permission reply.
    let native_browser_suspend = suspend_native_browser_views(parent, false);
    let pending = Rc::new(RefCell::new(Some((
        decision_handler,
        native_browser_suspend,
    ))));
    dialog.connect_response(None, move |_dialog, response| {
        if let Some((decision_handler, native_browser_suspend)) = pending.borrow_mut().take() {
            let decision = if response == "allow" {
                WKPermissionDecision::Grant
            } else {
                WKPermissionDecision::Deny
            };
            decision_handler.call((decision,));
            drop(native_browser_suspend);
        }
    });
    dialog.present(Some(parent));
}

enum PageDialog {
    Alert,
    Confirm,
    Prompt(String),
}

/// One-shot answer to a page dialog: `Some` for OK (with prompt text), `None`
/// for cancel. WebKit raises an exception when a panel completion handler is
/// released uncalled, so dropping an unanswered reply cancels the dialog.
struct PageDialogReply(Option<Box<dyn FnOnce(Option<String>)>>);

impl PageDialogReply {
    fn send(&mut self, reply: Option<String>) {
        if let Some(send) = self.0.take() {
            send(reply);
        }
    }
}

impl Drop for PageDialogReply {
    fn drop(&mut self) {
        self.send(None);
    }
}

fn present_page_dialog(
    ivars: &BrowserUIDelegateIvars,
    frame: &WKFrameInfo,
    message: &NSString,
    kind: PageDialog,
    reply: PageDialogReply,
) {
    let Some(parent) = ivars
        .placeholder
        .upgrade()
        .and_then(|widget| widget.root())
        .and_then(|root| root.downcast::<gtk::Window>().ok())
    else {
        return;
    };
    let site = unsafe { frame.securityOrigin().host().to_string() };
    let heading = if site.is_empty() {
        "This page says".to_string()
    } else {
        format!("{site} says")
    };
    let dialog = adw::AlertDialog::new(Some(&heading), Some(&message.to_string()));
    if !matches!(kind, PageDialog::Alert) {
        dialog.add_response("cancel", "Cancel");
    }
    dialog.add_response("ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response(if matches!(kind, PageDialog::Alert) {
        "ok"
    } else {
        "cancel"
    });
    let entry = match kind {
        PageDialog::Prompt(default_text) => {
            let entry = gtk::Entry::builder()
                .text(default_text)
                .activates_default(true)
                .build();
            dialog.set_extra_child(Some(&entry));
            Some(entry)
        }
        _ => None,
    };

    // A pending JavaScript dialog blocks WebKit's snapshot completion.
    let native_browser_suspend = suspend_native_browser_views(&parent, false);
    let pending = Rc::new(RefCell::new(Some((reply, native_browser_suspend))));
    let pending_on_unmap = pending.clone();
    dialog.connect_unmap(move |_| {
        // Window teardown can unmap a panel without sending a response.
        pending_on_unmap.borrow_mut().take();
    });
    dialog.connect_response(None, move |_dialog, response| {
        if let Some((mut reply, native_browser_suspend)) = pending.borrow_mut().take() {
            reply.send((response == "ok").then(|| {
                entry
                    .as_ref()
                    .map(|entry| entry.text().to_string())
                    .unwrap_or_default()
            }));
            drop(native_browser_suspend);
        }
    });
    dialog.present(Some(&parent));
}

fn website_data_store(
    mtm: MainThreadMarker,
    profile: &BrowserProfile,
    persist_session: bool,
) -> Retained<WKWebsiteDataStore> {
    if !persist_session {
        return unsafe { WKWebsiteDataStore::nonPersistentDataStore(mtm) };
    }

    let slug = profile.slug();
    let uuid = NSString::from_str(&uuid_for_profile_slug(&slug));
    match NSUUID::from_string(&uuid) {
        Some(identifier) => unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&identifier, mtm) },
        None => unsafe { WKWebsiteDataStore::defaultDataStore(mtm) },
    }
}

fn sync_native_view_frame(web_view: &WKWebView, placeholder: &gtk::Widget) {
    if !placeholder.is_mapped() {
        web_view.as_super().setHidden(true);
        return;
    }

    let Some(root) = placeholder.root() else {
        web_view.as_super().setHidden(true);
        return;
    };
    let Ok(window) = root.downcast::<gtk::Window>() else {
        web_view.as_super().setHidden(true);
        return;
    };
    let Some(content_view) = native_content_view(&window) else {
        web_view.as_super().setHidden(true);
        return;
    };
    let Some(bounds) = placeholder.compute_bounds(&window) else {
        web_view.as_super().setHidden(true);
        return;
    };

    let width = f64::from(bounds.width()).max(1.0);
    let height = f64::from(bounds.height()).max(1.0);
    let content_bounds = content_view.bounds();
    let x = f64::from(bounds.x());
    let y = if content_view.isFlipped() {
        f64::from(bounds.y())
    } else {
        content_bounds.size.height - f64::from(bounds.y()) - height
    };
    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
    let view = web_view.as_super();
    view.setFrame(frame);
    if unsafe { view.superview() }.is_none() {
        content_view.addSubview(view);
        view.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
    }
    if native_view_is_suspended(view) {
        view.setHidden(true);
        return;
    }
    view.setHidden(false);
}

fn native_content_view(window: &gtk::Window) -> Option<Retained<NSView>> {
    let surface = window.surface()?;
    let surface_ptr = surface.to_glib_none().0;
    let ns_window = unsafe { gdk_macos_surface_get_native_window(surface_ptr) };
    if ns_window.is_null() {
        return None;
    }
    unsafe { (&*(ns_window as *mut NSWindow)).contentView() }
}

fn native_view_has_focus(web_view: &WKWebView) -> bool {
    let web_view = web_view.as_super();
    let Some(responder) = web_view.window().and_then(|window| window.firstResponder()) else {
        return false;
    };
    let Some(responder_view) = responder.downcast_ref::<NSView>() else {
        return false;
    };
    std::ptr::eq(responder_view, web_view) || responder_view.isDescendantOf(web_view)
}

fn native_focus_entered(previous: &Cell<bool>, focused: bool) -> bool {
    let was_focused = previous.replace(focused);
    focused && !was_focused
}

fn focus_native_view(web_view: &WKWebView) {
    let view = web_view.as_super();
    if let Some(window) = view.window() {
        let responder: &NSResponder = view.as_super();
        window.makeFirstResponder(Some(responder));
    }
}

fn sync_browser_state(
    native: &NativeBrowserView,
    address: &gtk::Entry,
    id: PaneId,
    surface_id: SurfaceId,
    uri_cb: &Rc<RefCell<dyn FnMut(PaneId, SurfaceId, String)>>,
    title_cb: &Rc<RefCell<dyn FnMut(PaneId, SurfaceId, String)>>,
) {
    let url = current_url_native(&native.web_view);
    if !url.is_empty() && *native.last_url.borrow() != url {
        address.set_text(&url);
        *native.last_url.borrow_mut() = url.clone();
        (uri_cb.borrow_mut())(id, surface_id, url);
    }

    let title = current_title_native(&native.web_view);
    if !title.trim().is_empty() && *native.last_title.borrow() != title {
        *native.last_title.borrow_mut() = title.clone();
        (title_cb.borrow_mut())(id, surface_id, title);
    }
}

fn current_url_native(web_view: &WKWebView) -> String {
    unsafe {
        web_view
            .URL()
            .and_then(|url| url.absoluteString())
            .map(|url| url.to_string())
    }
    .unwrap_or_default()
}

fn current_title_native(web_view: &WKWebView) -> String {
    unsafe { web_view.title().map(|title| title.to_string()) }.unwrap_or_default()
}

fn load_uri_native(web_view: &WKWebView, url: &str) {
    if url == "about:blank" {
        let html = NSString::from_str("<!doctype html><meta charset=\"utf-8\"><title></title>");
        unsafe {
            web_view.loadHTMLString_baseURL(&html, None);
        }
        return;
    }

    let ns_url = NSString::from_str(url);
    let Some(url) = NSURL::URLWithString(&ns_url) else {
        tracing::warn!(url, "WKWebView rejected URL");
        return;
    };
    let request = NSURLRequest::requestWithURL(&url);
    unsafe {
        web_view.loadRequest(&request);
    }
}

unsafe fn save_nsimage_to_png(
    image: &objc2_app_kit::NSImage,
    path: &Path,
    capture: Option<flowmux_browser::snapshot::CaptureSize>,
) -> Result<String, String> {
    let tiff = image
        .TIFFRepresentation()
        .ok_or_else(|| "snapshot image had no TIFF representation".to_string())?;
    let bitmap = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &tiff)
        .ok_or_else(|| "snapshot image could not be converted to bitmap".to_string())?;
    let properties = NSDictionary::<objc2_app_kit::NSBitmapImageRepPropertyKey, AnyObject>::new();
    let png = bitmap
        .representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
        .ok_or_else(|| "snapshot bitmap could not be encoded as PNG".to_string())?;
    let png = if let Some(size) = capture.filter(|size| size.clip.is_some()) {
        let cropped = crate::ui::browser_capture::crop_png(png.as_bytes_unchecked(), size)?;
        objc2_foundation::NSData::with_bytes(&cropped)
    } else {
        png
    };
    let path_string = NSString::from_str(&path.to_string_lossy());
    if png.writeToFile_atomically(&path_string, true) {
        Ok(path.display().to_string())
    } else {
        Err(format!("failed to write snapshot PNG: {}", path.display()))
    }
}

fn object_description(value: *mut AnyObject) -> String {
    if value.is_null() {
        return String::new();
    }
    autoreleasepool(|_| unsafe {
        let value = &*value;
        let description: Retained<NSString> = msg_send![value, description];
        description.to_string()
    })
}

fn error_description(error: *mut NSError) -> String {
    if error.is_null() {
        return "unknown WebKit error".into();
    }
    unsafe {
        let error = &*error;
        let description = error.localizedDescription().to_string();
        if let Some(message) = error
            .userInfo()
            .objectForKey(&NSString::from_str("WKJavaScriptExceptionMessage"))
        {
            if let Some(message) = message.downcast_ref::<NSString>() {
                return format!("{description}: {message}");
            }
        }
        description
    }
}

fn normalize_uri(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return "about:blank".into();
    }
    if raw.starts_with("http://")
        || raw.starts_with("https://")
        || raw.starts_with("about:")
        || raw.starts_with("file://")
    {
        return raw.to_string();
    }
    if raw.starts_with("localhost") || raw.starts_with("127.") || raw.starts_with("[::1]") {
        return format!("http://{raw}");
    }
    if raw.contains('.') && !raw.contains(' ') {
        return format!("https://{raw}");
    }
    format!("https://duckduckgo.com/?q={}", urlencode(raw))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".into(),
            other => format!("%{:02X}", other),
        })
        .collect()
}

fn set_native_zoom(native: &NativeBrowserView, label: &gtk::Button, requested: f64) {
    let level = requested.clamp(0.5, 2.0);
    native.zoom.set(level);
    label.set_label(&format!("{:.0}%", level * 100.0));
    unsafe {
        native.web_view.setPageZoom(level);
    }
}

fn engine_to_profile(engine: &BrowserEngine) -> BrowserProfile {
    match engine {
        BrowserEngine::Webkit => BrowserProfile::Default,
        BrowserEngine::Chrome => BrowserProfile::ChromeImport,
        BrowserEngine::Firefox => BrowserProfile::FirefoxImport,
        BrowserEngine::Custom { name } => BrowserProfile::Custom { name: name.clone() },
    }
}

fn uuid_for_profile_slug(slug: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in b"flowmux-browser-profile:" {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    for byte in slug.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&0x666c6f776d757800u64.to_be_bytes());
    bytes[8..].copy_from_slice(&hash.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_dialog_reply_cancels_on_drop_and_answers_only_once() {
        let replies = Rc::new(RefCell::new(Vec::new()));
        {
            let replies = replies.clone();
            let _reply = PageDialogReply(Some(Box::new(move |value| {
                replies.borrow_mut().push(value);
            })));
        }
        {
            let replies = replies.clone();
            let mut reply = PageDialogReply(Some(Box::new(move |value| {
                replies.borrow_mut().push(value);
            })));
            reply.send(Some("edited".into()));
            reply.send(None);
        }
        assert_eq!(*replies.borrow(), [None, Some("edited".into())]);
    }

    #[test]
    fn navigation_delegate_class_registers_with_native_ivar_alignment() {
        let _ = BrowserNavigationDelegate::class();
    }

    #[test]
    fn native_focus_reports_each_browser_entry() {
        let focused = Cell::new(false);

        assert!(native_focus_entered(&focused, true));
        assert!(!native_focus_entered(&focused, true));
        assert!(!native_focus_entered(&focused, false));
        assert!(native_focus_entered(&focused, true));
    }

    #[test]
    fn normalize_uri_matches_webkit_backend_rules() {
        assert_eq!(normalize_uri(""), "about:blank");
        assert_eq!(normalize_uri("localhost:3000"), "http://localhost:3000");
        assert_eq!(normalize_uri("example.com"), "https://example.com");
        assert_eq!(
            normalize_uri("rust webkit"),
            "https://duckduckgo.com/?q=rust+webkit"
        );
    }

    #[test]
    fn profile_uuid_is_stable_and_uuid_shaped() {
        let first = uuid_for_profile_slug("default");
        let second = uuid_for_profile_slug("default");
        assert_eq!(first, second);
        assert_eq!(first.len(), 36);
        assert_eq!(&first[14..15], "5");
    }

    #[test]
    fn media_capture_names_distinguish_webkit_request_types() {
        assert_eq!(
            media_capture_name(WKMediaCaptureType::Camera),
            "camera access"
        );
        assert_eq!(
            media_capture_name(WKMediaCaptureType::Microphone),
            "microphone access"
        );
        assert_eq!(
            media_capture_name(WKMediaCaptureType::CameraAndMicrophone),
            "camera and microphone access"
        );
    }

    #[test]
    fn download_path_sanitizes_names_and_avoids_collisions() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("report.txt"), b"first").unwrap();

        assert_eq!(
            available_download_path(directory.path(), "../report.txt"),
            directory.path().join("report (1).txt")
        );
        assert_eq!(
            available_download_path(directory.path(), "../../"),
            directory.path().join("download")
        );
    }
}
