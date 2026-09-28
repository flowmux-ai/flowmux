// SPDX-License-Identifier: GPL-3.0-or-later
//! Deferred native popups. Every COM object and queue stays on the host UI thread.
use super::*;
use std::{
    cell::Cell,
    collections::VecDeque,
    rc::{Rc, Weak},
};
use webview2_com::{Microsoft::Web::WebView2::Win32::*, NewWindowRequestedEventHandler};
use windows::core::Interface;

pub(crate) const MAX_PENDING: usize = 8;
const DEADLINE: Duration = Duration::from_secs(12);

struct Native {
    surface: SurfaceId,
    args: ICoreWebView2NewWindowRequestedEventArgs,
    deferral: RefCell<Option<ICoreWebView2Deferral>>,
}
impl Native {
    fn pending(&self) -> bool {
        self.deferral.borrow().is_some()
    }
    fn cancel(&self) -> anyhow::Result<()> {
        // Complete can reenter WebView callbacks: release the RefCell borrow first.
        let deferral = self.deferral.borrow_mut().take();
        if let Some(deferral) = deferral {
            unsafe {
                let handled = self.args.SetHandled(true);
                let completed = deferral.Complete();
                handled?;
                completed?;
            }
        }
        Ok(())
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}

#[derive(Default)]
struct Shared {
    queue: RefCell<VecDeque<Request>>,
    // Draining the queue must not stop shutdown from completing its deferrals.
    active: RefCell<HashMap<Uuid, Weak<Native>>>,
    closed: Cell<bool>,
    opened: Cell<u64>,
    rejected: Cell<u64>,
    last_error: RefCell<Option<String>>,
}
impl Shared {
    fn reject(&self, reason: &str) {
        self.rejected.set(self.rejected.get().saturating_add(1));
        *self.last_error.borrow_mut() = Some(reason.chars().take(1024).collect());
    }
}

pub(crate) struct Request {
    pub(crate) surface: SurfaceId,
    pub(crate) navigation: u64,
    pub(crate) visibility_revision: u64,
    pub(crate) uri: String,
    pub(crate) user_initiated: bool,
    pub(crate) started: Instant,
    id: Uuid,
    environment: ICoreWebView2Environment,
    epoch: Arc<AtomicU64>,
    visibility: Rc<Cell<Option<u64>>>,
    source_closed: Rc<Cell<bool>>,
    native: Rc<Native>,
    shared: Weak<Shared>,
}
impl Request {
    /// Use with Wry's with_environment; omit with_url/with_html for the child.
    pub(crate) fn environment(&self) -> ICoreWebView2Environment {
        self.environment.clone()
    }
    /// Recheck after creating the child: Wry creation pumps the UI message loop.
    pub(crate) fn valid(&self) -> bool {
        self.shared
            .upgrade()
            .is_some_and(|shared| !shared.closed.get())
            && !self.source_closed.get()
            && self.epoch.load(Ordering::SeqCst) == self.navigation
            && self.visibility.get() == Some(self.visibility_revision)
            && self.started.elapsed() <= DEADLINE
            && self.native.pending()
    }
    /// Attach a fresh, un-navigated WebView in the opener's environment/profile.
    /// SetNewWindow retains the native WindowProxy/opener relationship; loading
    /// the URI manually cannot replace this operation.
    pub(crate) fn attach(&mut self, child: &ICoreWebView2) -> anyhow::Result<()> {
        anyhow::ensure!(self.valid(), "popup request expired or opener changed");
        unsafe {
            self.native.args.SetHandled(true)?;
            self.native.args.SetNewWindow(child)?;
        }
        let deferral = self
            .native
            .deferral
            .borrow_mut()
            .take()
            .context("popup request completed during native attachment")?;
        unsafe {
            deferral.Complete()?;
        }
        Ok(())
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            shared.active.borrow_mut().remove(&self.id);
        }
        let _ = self.native.cancel();
    }
}

#[derive(Default)]
pub(crate) struct Controller {
    shared: Rc<Shared>,
    surface_guards: HashMap<SurfaceId, Rc<Cell<bool>>>,
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.shutdown();
    }
}
impl Controller {
    /// Call after Wry construction with no with_new_window_req_handler set.
    /// Wry's default handler sets Handled synchronously; a custom Wry callback
    /// would introduce a second deferred decision that can override this one.
    pub(crate) fn install(
        &mut self,
        core: &ICoreWebView2,
        surface: SurfaceId,
        epoch: Arc<AtomicU64>,
        visibility: Rc<Cell<Option<u64>>>,
        sender: EventSender,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!self.shared.closed.get(), "popup controller is closed");
        anyhow::ensure!(
            !self.surface_guards.contains_key(&surface),
            "popup handler is already installed for this surface"
        );
        let environment = unsafe { core.cast::<ICoreWebView2_2>()?.Environment()? };
        let shared = self.shared.clone();
        let source_closed = Rc::new(Cell::new(false));
        let guard = source_closed.clone();
        unsafe {
            core.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    // Suppress uncontrolled native windows on every rejection path.
                    if let Err(error) = args.SetHandled(true) {
                        shared.reject(&format!("cannot handle popup request: {error}"));
                        return Err(error);
                    }
                    if shared.closed.get() || guard.get() {
                        shared.reject("popup opener or controller is closed");
                        return Ok(());
                    }
                    let Some(visibility_revision) = visibility.get() else {
                        shared.reject("popup opener is hidden");
                        return Ok(());
                    };
                    if shared.active.borrow().len() >= MAX_PENDING {
                        shared.reject("at most eight popup requests may be pending");
                        return Ok(());
                    }
                    let mut uri = Default::default();
                    if let Err(error) = args.Uri(&mut uri) {
                        shared.reject(&format!("cannot read popup URI: {error}"));
                        return Err(error);
                    }
                    let uri = webview2_com::take_pwstr(uri);
                    if uri.len() > crate::browser::MAX_URL_BYTES {
                        shared.reject("popup URI exceeds 16 KiB");
                        return Ok(());
                    }
                    let mut user_initiated = Default::default();
                    if let Err(error) = args.IsUserInitiated(&mut user_initiated) {
                        shared.reject(&format!("cannot read popup initiation: {error}"));
                        return Err(error);
                    }
                    let deferral = match args.GetDeferral() {
                        Ok(deferral) => deferral,
                        Err(error) => {
                            shared.reject(&format!("cannot defer popup request: {error}"));
                            return Err(error);
                        }
                    };
                    let native = Rc::new(Native {
                        surface,
                        args,
                        deferral: RefCell::new(Some(deferral)),
                    });
                    let id = Uuid::new_v4();
                    let request = Request {
                        surface,
                        navigation: epoch.load(Ordering::SeqCst),
                        visibility_revision,
                        uri,
                        user_initiated: user_initiated.as_bool(),
                        started: Instant::now(),
                        id,
                        environment: environment.clone(),
                        epoch: epoch.clone(),
                        visibility: visibility.clone(),
                        source_closed: guard.clone(),
                        native: native.clone(),
                        shared: Rc::downgrade(&shared),
                    };
                    shared
                        .active
                        .borrow_mut()
                        .insert(id, Rc::downgrade(&native));
                    shared.queue.borrow_mut().push_back(request);
                    // Creating WebViews inside NewWindowRequested can deadlock.
                    // Only metadata crosses the host event channel, never COM.
                    sender.send(Event::Browser(Signal::Popup));
                    Ok(())
                })),
                &mut 0,
            )?;
        }
        self.surface_guards.insert(surface, source_closed);
        Ok(())
    }

    pub(crate) fn drain(&mut self) -> Vec<Request> {
        let queued = std::mem::take(&mut *self.shared.queue.borrow_mut());
        queued.into_iter().collect()
    }

    /// Permanently reject this surface's future requests before dropping its view.
    pub(crate) fn cancel_surface(&mut self, surface: SurfaceId) {
        if let Some(guard) = self.surface_guards.remove(&surface) {
            guard.set(true);
        }
        let pending: Vec<_> = self
            .shared
            .active
            .borrow()
            .values()
            .filter_map(Weak::upgrade)
            .filter(|native| native.surface == surface)
            .collect();
        let queued = std::mem::take(&mut *self.shared.queue.borrow_mut());
        let (cancelled, retained): (VecDeque<_>, VecDeque<_>) = queued
            .into_iter()
            .partition(|request| request.surface == surface);
        self.shared.queue.borrow_mut().extend(retained);
        for native in pending {
            if native.pending() {
                self.shared.reject("popup opener closed before attachment");
                let _ = native.cancel();
            }
        }
        drop(cancelled);
    }

    /// Must run before browser WebViews are cleared, including on host shutdown.
    pub(crate) fn shutdown(&mut self) {
        if self.shared.closed.replace(true) {
            return;
        }
        for (_, guard) in self.surface_guards.drain() {
            guard.set(true);
        }
        let pending: Vec<_> = self
            .shared
            .active
            .borrow()
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        let queued = std::mem::take(&mut *self.shared.queue.borrow_mut());
        for native in pending {
            let _ = native.cancel();
        }
        drop(queued);
    }

    pub(crate) fn rejected(&self, reason: &str) {
        self.shared.reject(reason);
    }

    pub(crate) fn opened(&self) {
        self.shared
            .opened
            .set(self.shared.opened.get().saturating_add(1));
    }

    pub(crate) fn status(&self) -> Value {
        json!({
            "pending": self.shared.active.borrow().len(),
            "opened": self.shared.opened.get(),
            "rejected": self.shared.rejected.get(),
            "last_error": self.shared.last_error.borrow().as_ref(),
        })
    }
}
