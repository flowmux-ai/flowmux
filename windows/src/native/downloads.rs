// SPDX-License-Identifier: GPL-3.0-or-later
//! Native downloads, isolated staging, and publication without replacement.
use super::*;
use crate::downloads::{self as domain, Op};
use std::{cell::Cell, path::Path, rc::Rc};
use webview2_com::{
    DownloadStartingEventHandler, Microsoft::Web::WebView2::Win32::*, StateChangedEventHandler,
};
use windows::core::{Interface, PCWSTR};
use windows_sys::Win32::{
    Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH},
    UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, ShellExecuteW},
};
#[path = "download_panel.rs"]
mod panel;
pub(super) use panel::UiAction;

pub(super) enum Signal {
    Start,
    Rejected,
    Interrupted(Uuid, i32),
    Prepared(Uuid, Result<Staging, String>),
    Finished(Uuid, Result<Option<PathBuf>, String>),
    Ui(UiAction),
}
struct Starting {
    surface: SurfaceId,
    navigation: u64,
    args: ICoreWebView2DownloadStartingEventArgs,
    operation: ICoreWebView2DownloadOperation,
    deferral: Option<ICoreWebView2Deferral>,
    state_token: Option<i64>,
}
impl Starting {
    fn observe(&mut self, id: Uuid, sender: EventSender) -> anyhow::Result<()> {
        let stopped = Cell::new(false);
        let mut token = 0;
        unsafe {
            self.operation.add_StateChanged(
                &StateChangedEventHandler::create(Box::new(move |operation, _| {
                    let Some(operation) = operation else {
                        return Ok(());
                    };
                    let mut state = Default::default();
                    operation.State(&mut state)?;
                    if state == COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED && !stopped.replace(true) {
                        let mut reason = Default::default();
                        operation.InterruptReason(&mut reason)?;
                        sender.send(Event::Download(Signal::Interrupted(id, reason.0)));
                        // Cancel a reported interruption. The runtime can also
                        // retry internally before exposing an interrupted state.
                        if reason != COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_CANCELED {
                            operation.Cancel()?;
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        self.state_token = Some(token);
        Ok(())
    }
    fn cancel(&mut self) -> anyhow::Result<()> {
        unsafe {
            if let Some(deferral) = self.deferral.as_ref() {
                self.args.SetCancel(true)?;
                deferral.Complete()?;
                self.deferral.take();
                return Ok(());
            }
            self.operation.Cancel()?;
        }
        Ok(())
    }
    fn start(&mut self, path: &Path) -> anyhow::Result<()> {
        unsafe {
            // Follow Wry's download adapter: give WebView2 an ordinary Win32
            // path instead of the extended namespace returned by canonicalize.
            let text = path.to_str().context("download path is not Unicode")?;
            let text = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
                format!(r"\\{rest}")
            } else {
                text.strip_prefix(r"\\?\").unwrap_or(text).to_string()
            };
            self.args.SetResultFilePath(PCWSTR(wide(text).as_ptr()))?;
            self.args.SetCancel(false)?;
            self.deferral
                .as_ref()
                .context("download deferral is no longer available")?
                .Complete()?;
            self.deferral.take();
        }
        Ok(())
    }
}
impl Drop for Starting {
    fn drop(&mut self) {
        if let Some(token) = self.state_token.take() {
            unsafe {
                let _ = self.operation.remove_StateChanged(token);
            }
        }
        if let Some(deferral) = self.deferral.take() {
            unsafe {
                let _ = self.args.SetCancel(true);
                let _ = deferral.Complete();
            }
        }
    }
}
#[derive(Debug, Clone)]
pub(super) struct Staging {
    root: PathBuf,
    directory: PathBuf,
    path: PathBuf,
}
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Preparing,
    Downloading,
    Cancelling,
    Finalizing,
    Complete,
    Cancelled,
    Failed,
}
impl Phase {
    fn active(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Downloading | Self::Cancelling | Self::Finalizing
        )
    }
}
#[derive(serde::Serialize)]
struct Record {
    id: Uuid,
    surface: SurfaceId,
    navigation: u64,
    filename: String,
    uri: String,
    phase: Phase,
    received: u64,
    total: Option<u64>,
    path: Option<PathBuf>,
    error: Option<String>,
}
struct Entry {
    record: Record,
    native: Option<Starting>,
    staging: Option<Staging>,
    worker: bool,
    cancel: bool,
    cancel_accepted: bool,
    started: Instant,
}
#[derive(Default)]
pub(super) struct Controller {
    queue: Rc<RefCell<Vec<Starting>>>,
    closed: Rc<Cell<bool>>,
    surface_guards: HashMap<SurfaceId, Rc<Cell<bool>>>,
    entries: Vec<Entry>,
    panel: Option<panel::Panel>,
    rejected: u64,
}
impl Drop for Controller {
    fn drop(&mut self) {
        // WebView callbacks retain the queue Rc after the controller is dropped.
        // Reject reentrant starts, then release pending COM objects while their
        // WebView owners are still alive, without holding a RefCell borrow.
        self.closed.set(true);
        let pending = std::mem::take(&mut *self.queue.borrow_mut());
        drop(pending);
        for entry in &mut self.entries {
            if entry.record.phase.active() {
                if let Some(native) = &mut entry.native {
                    let _ = native.cancel();
                }
            }
        }
    }
}
impl Controller {
    pub(super) fn handle_message(&self, msg: &MSG) -> bool {
        self.panel.as_ref().is_some_and(|p| p.handle_message(msg))
    }
    pub(super) fn install(
        &mut self,
        core: &ICoreWebView2,
        surface: SurfaceId,
        navigation: Arc<std::sync::atomic::AtomicU64>,
        sender: EventSender,
    ) -> anyhow::Result<()> {
        let core: ICoreWebView2_4 = core.cast()?;
        let queue = self.queue.clone();
        let closed = self.closed.clone();
        let surface_closed = Rc::new(Cell::new(false));
        let callback_guard = surface_closed.clone();
        unsafe {
            core.add_DownloadStarting(
                &DownloadStartingEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    // Cancel by default. No native dialog or original destination is
                    // permitted while the host prepares its own staging location.
                    args.SetCancel(true)?;
                    args.SetHandled(true)?;
                    if closed.get() || callback_guard.get() {
                        return Ok(());
                    }
                    if queue.borrow().len() >= domain::MAX_ACTIVE {
                        sender.send(Event::Download(Signal::Rejected));
                        return Ok(());
                    }
                    let operation = args.DownloadOperation()?;
                    let deferral = args.GetDeferral()?;
                    queue.borrow_mut().push(Starting {
                        surface,
                        navigation: navigation.load(std::sync::atomic::Ordering::SeqCst),
                        args,
                        operation,
                        deferral: Some(deferral),
                        state_token: None,
                    });
                    sender.send(Event::Download(Signal::Start));
                    Ok(())
                })),
                &mut 0,
            )?;
        }
        self.surface_guards.insert(surface, surface_closed);
        Ok(())
    }
    fn rows(&self) -> Vec<Value> {
        self.entries
            .iter()
            .map(|e| serde_json::to_value(&e.record).unwrap())
            .collect()
    }
    fn status(&self) -> Value {
        json!({"entries":self.rows(),"active":self.entries.iter().filter(|e|e.record.phase.active()).count(),"active_limit":domain::MAX_ACTIVE,"retained_limit":domain::MAX_ROWS,"rejected_at_capacity":self.rejected,
            "panel_handle":self.panel.as_ref().map(|p|p.window as usize),"panel_owner":self.panel.as_ref().map(|p|p.owner() as usize),"panel_rows":self.panel.as_ref().map(|p|p.rows()),"panel_status":self.panel.as_ref().map(|p|p.status_text())})
    }
    fn refresh(&self) {
        if let Some(panel) = &self.panel {
            panel.results(&self.rows());
        }
    }
    fn trim(&mut self) {
        while self.entries.len() >= domain::MAX_ROWS {
            let Some(index) = self.entries.iter().position(|e| !e.record.phase.active()) else {
                break;
            };
            self.entries.remove(index);
        }
    }
}

fn root(background: bool) -> anyhow::Result<PathBuf> {
    if background {
        return Ok(PathBuf::from(
            std::env::var_os("FLOWMUX_TEST_STATE_DIR")
                .context("background downloads require isolated state")?,
        )
        .join("downloads"));
    }
    unsafe {
        let mut value = std::ptr::null_mut();
        let result = SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut value);
        anyhow::ensure!(
            result >= 0 && !value.is_null(),
            "Windows Downloads folder is unavailable ({result:#x})"
        );
        let length = (0..32768).take_while(|i| *value.add(*i) != 0).count();
        let text = String::from_utf16(std::slice::from_raw_parts(value, length));
        CoTaskMemFree(value.cast());
        Ok(PathBuf::from(text?))
    }
}
fn prepare(background: bool, id: Uuid, filename: &str) -> anyhow::Result<Staging> {
    let root = root(background)?;
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let directory = root.join(format!(".flowmux-download-{id}"));
    std::fs::create_dir(&directory)?;
    Ok(Staging {
        path: directory.join(filename),
        root,
        directory,
    })
}
fn cleanup(staging: &Staging) -> anyhow::Result<()> {
    // WebView2 can briefly retain its file handle after cancellation. Retry only
    // cleanup of this UUID-owned staging directory, never the network request.
    for attempt in 0..10 {
        match std::fs::remove_dir_all(&staging.directory) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) if attempt < 9 => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!()
}
fn publish(staging: &Staging, filename: &str) -> anyhow::Result<PathBuf> {
    for index in 0..10000 {
        let path = staging.root.join(domain::candidate(filename, index));
        if path.try_exists()? {
            continue;
        }
        let result = unsafe {
            checked(MoveFileExW(
                wide(&staging.path).as_ptr(),
                wide(&path).as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            ))
        };
        match result {
            Ok(()) => {
                let _ = std::fs::remove_dir(&staging.directory);
                return Ok(path);
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| matches!(e.raw_os_error(), Some(80 | 183))) =>
            {
                continue
            }
            Err(error) => {
                return Err(error).context("publishing completed download without replacement")
            }
        }
    }
    anyhow::bail!(
        "all 10000 download filename candidates exist; completed data retained in staging"
    )
}
fn metadata(native: &Starting) -> anyhow::Result<(String, String, Option<u64>)> {
    unsafe {
        let mut path = Default::default();
        native.args.ResultFilePath(&mut path)?;
        let fallback = webview2_com::take_pwstr(path);
        let mut disposition = Default::default();
        native.operation.ContentDisposition(&mut disposition)?;
        let filename = domain::suggested(&webview2_com::take_pwstr(disposition), &fallback);
        let mut uri = Default::default();
        native.operation.Uri(&mut uri)?;
        let uri = webview2_com::take_pwstr(uri).chars().take(16384).collect();
        let mut total = -1;
        native.operation.TotalBytesToReceive(&mut total)?;
        Ok((filename, uri, u64::try_from(total).ok()))
    }
}

impl App {
    pub(super) fn download_event(&mut self, event: Signal) {
        match event {
            Signal::Interrupted(id, reason) => {
                if let Some(entry) = self
                    .downloads
                    .entries
                    .iter_mut()
                    .find(|e| e.record.id == id && e.record.phase.active())
                {
                    if !entry.cancel {
                        entry.record.error =
                            Some(format!("WebView2 download interrupted ({reason})"));
                        entry.cancel = true;
                        entry.record.phase = Phase::Cancelling;
                    }
                    if let Some(native) = &mut entry.native {
                        entry.cancel_accepted |= native.cancel().is_ok();
                    }
                }
                self.download_tick();
            }
            Signal::Rejected => {
                self.downloads.rejected = self.downloads.rejected.saturating_add(1);
            }
            Signal::Ui(action) => {
                if let Err(e) = self.download_ui(action) {
                    if let Some(panel) = &self.downloads.panel {
                        panel.status(&e.to_string());
                    }
                }
            }
            Signal::Start => {
                let starts = std::mem::take(&mut *self.downloads.queue.borrow_mut());
                for mut native in starts {
                    if !self.browsers.contains_key(&native.surface) {
                        continue;
                    }
                    let (filename, uri, total) = match metadata(&native) {
                        Ok(data) => data,
                        Err(e) => {
                            report(&format!("download metadata: {e:#}"));
                            continue;
                        }
                    };
                    // Some runtimes emit a new operation for a network retry of
                    // an incomplete attachment. Treat the navigation/URI as one
                    // transfer: cancel the restart and fail its original record.
                    // A deliberate retry through a new navigation has a new ID.
                    if let Some(entry) = self.downloads.entries.iter_mut().find(|e| {
                        e.record.surface == native.surface
                            && e.record.navigation == native.navigation
                            && e.record.uri == uri
                            && e.record.phase != Phase::Complete
                    }) {
                        let _ = native.cancel();
                        if !entry.cancel && entry.record.phase != Phase::Finalizing {
                            entry.record.error = Some(
                                "native download restarted an incomplete transfer; start a new navigation to retry".into(),
                            );
                            entry.cancel = true;
                            if entry.record.phase.active() {
                                entry.record.phase = Phase::Cancelling;
                                if let Some(original) = &mut entry.native {
                                    entry.cancel_accepted |= original.cancel().is_ok();
                                }
                            }
                        }
                        continue;
                    }
                    if self
                        .downloads
                        .entries
                        .iter()
                        .filter(|e| e.record.phase.active())
                        .count()
                        >= domain::MAX_ACTIVE
                    {
                        self.downloads.rejected = self.downloads.rejected.saturating_add(1);
                        continue;
                    }
                    self.downloads.trim();
                    let id = Uuid::new_v4();
                    if let Err(error) = native.observe(id, self.sender.clone()) {
                        report(&format!("download observer: {error:#}"));
                        continue;
                    }
                    let surface = native.surface;
                    let background = self.background_test;
                    let sender = self.sender.clone();
                    let name = filename.clone();
                    let worker = std::thread::Builder::new()
                        .name("download-prepare".into())
                        .spawn(move || {
                            sender.send(Event::Download(Signal::Prepared(
                                id,
                                prepare(background, id, &name).map_err(|e| format!("{e:#}")),
                            )));
                        });
                    let mut entry = Entry {
                        record: Record {
                            id,
                            surface,
                            navigation: native.navigation,
                            filename,
                            uri,
                            phase: Phase::Preparing,
                            received: 0,
                            total,
                            path: None,
                            error: None,
                        },
                        native: Some(native),
                        staging: None,
                        worker: worker.is_ok(),
                        cancel: false,
                        cancel_accepted: false,
                        started: Instant::now(),
                    };
                    if let Err(error) = worker {
                        entry.native.take();
                        entry.record.phase = Phase::Failed;
                        entry.record.error = Some(error.to_string());
                    }
                    self.downloads.entries.push(entry);
                }
            }
            Signal::Prepared(id, result) => {
                if let Some(entry) = self
                    .downloads
                    .entries
                    .iter_mut()
                    .find(|e| e.record.id == id)
                {
                    entry.worker = false;
                    match result {
                        Ok(staging) => {
                            entry.record.path = Some(staging.path.clone());
                            entry.staging = Some(staging);
                            if !entry.cancel {
                                let result = entry
                                    .native
                                    .as_mut()
                                    .context("download was closed")
                                    .and_then(|n| n.start(&entry.staging.as_ref().unwrap().path));
                                if let Err(error) = result {
                                    entry.record.error = Some(error.to_string());
                                    entry.cancel = true;
                                    if let Some(n) = &mut entry.native {
                                        entry.cancel_accepted = n.cancel().is_ok();
                                    }
                                } else {
                                    entry.record.phase = Phase::Downloading;
                                }
                            }
                        }
                        Err(error) => {
                            entry.native.take();
                            entry.record.phase = if entry.cancel {
                                Phase::Cancelled
                            } else {
                                Phase::Failed
                            };
                            entry.record.error = Some(error);
                        }
                    }
                }
            }
            Signal::Finished(id, result) => {
                if let Some(entry) = self
                    .downloads
                    .entries
                    .iter_mut()
                    .find(|e| e.record.id == id)
                {
                    entry.worker = false;
                    entry.native.take();
                    match result {
                        Ok(Some(path)) => {
                            entry.record.path = Some(path);
                            entry.record.phase = Phase::Complete;
                            entry.staging = None;
                        }
                        Ok(None) => {
                            entry.record.path = None;
                            entry.staging = None;
                            entry.record.phase = if entry.record.error.is_some() {
                                Phase::Failed
                            } else {
                                Phase::Cancelled
                            };
                        }
                        Err(error) => {
                            entry.record.phase = Phase::Failed;
                            entry.record.error = Some(format!(
                                "{error}; staging retained at {}",
                                entry
                                    .record
                                    .path
                                    .as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default()
                            ));
                        }
                    }
                }
            }
        }
        self.downloads.refresh();
    }
    fn download_cancel(&mut self, id: Uuid) -> anyhow::Result<bool> {
        let entry = self
            .downloads
            .entries
            .iter_mut()
            .find(|e| e.record.id == id)
            .context("download not found")?;
        anyhow::ensure!(
            entry.record.phase != Phase::Finalizing,
            "file publication already started; cancellation is too late"
        );
        if !entry.record.phase.active() || entry.cancel {
            return Ok(false);
        }
        let previous_phase = entry.record.phase;
        entry.cancel = true;
        entry.record.phase = Phase::Cancelling;
        if let Some(native) = &mut entry.native {
            if let Err(error) = native.cancel() {
                // A rejected native request must allow the user to retry.
                entry.cancel = false;
                entry.record.phase = previous_phase;
                return Err(error);
            }
            entry.cancel_accepted = true;
        }
        Ok(true)
    }
    pub(super) fn download_cancel_surface(&mut self, surface: SurfaceId) {
        if let Some(closed) = self.downloads.surface_guards.remove(&surface) {
            closed.set(true);
        }
        let ids: Vec<_> = self
            .downloads
            .entries
            .iter()
            .filter(|e| e.record.surface == surface && e.record.phase.active())
            .map(|e| e.record.id)
            .collect();
        for id in ids {
            let _ = self.download_cancel(id);
        }
        // The owning WebView controller is about to close. Its old runtime
        // download objects must not be polled/released after controller.Close.
        let pending = std::mem::take(&mut *self.downloads.queue.borrow_mut());
        for native in pending {
            if native.surface != surface {
                self.downloads.queue.borrow_mut().push(native);
            }
            // Matching entries drop outside the queue borrow. A deferral's
            // completion may synchronously deliver another native callback.
        }
        for entry in &mut self.downloads.entries {
            if entry.record.surface == surface
                && entry.record.phase.active()
                && entry.record.phase != Phase::Finalizing
            {
                // Closing the owner is terminal even if its Cancel API rejected
                // the request; no native handle may survive controller.Close.
                entry.cancel = true;
                entry.record.phase = Phase::Cancelling;
                entry.native.take();
                entry.cancel_accepted = true;
            }
        }
        self.download_tick();
    }
    pub(super) fn download_tick(&mut self) {
        for entry in &mut self.downloads.entries {
            if !entry.record.phase.active() || entry.record.phase == Phase::Finalizing {
                continue;
            }
            if entry.worker {
                if entry.record.phase == Phase::Preparing
                    && entry.started.elapsed() > Duration::from_secs(15)
                {
                    if let Some(native) = &mut entry.native {
                        entry.cancel_accepted |= native.cancel().is_ok();
                    }
                    entry.cancel = true;
                    entry.record.phase = Phase::Cancelling;
                    entry.record.error = Some(
                        "download destination preparation timed out; waiting for file worker"
                            .into(),
                    );
                }
                continue;
            }
            let status = if entry.cancel && entry.cancel_accepted {
                // Do not call the old operation again after accepted cancellation
                // or after its owner has started closing.
                Ok(COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED.0)
            } else {
                let Some(native) = &entry.native else {
                    continue;
                };
                (|| -> anyhow::Result<i32> {
                    unsafe {
                        let mut received = 0;
                        native.operation.BytesReceived(&mut received)?;
                        entry.record.received = u64::try_from(received).unwrap_or(0);
                        let mut state = Default::default();
                        native.operation.State(&mut state)?;
                        if state == COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED && !entry.cancel {
                            let mut reason = Default::default();
                            native.operation.InterruptReason(&mut reason)?;
                            entry.record.error =
                                Some(format!("WebView2 download interrupted ({})", reason.0));
                        }
                        Ok(state.0)
                    }
                })()
            };
            // A successful Cancel need not produce another observable State.
            // Confirm cancellation by releasing native references and removing
            // only this transfer's staging directory, rather than waiting forever.
            let finished = match status {
                _ if entry.cancel && entry.cancel_accepted => true,
                Ok(state) if state == COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS.0 => false,
                Ok(state) => {
                    if state != COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED.0 && !entry.cancel {
                        entry.cancel = true;
                    }
                    true
                }
                Err(error) => {
                    if !entry.cancel {
                        entry.record.error = Some(error.to_string());
                    }
                    entry.cancel = true;
                    true
                }
            };
            if !finished {
                continue;
            }
            // Stop native retries before cleanup. Completed bytes are published
            // only if cancellation did not win before this transition.
            if entry.cancel {
                if let Some(native) = &mut entry.native {
                    let _ = native.cancel();
                }
            }
            let Some(staging) = entry.staging.clone() else {
                continue;
            };
            // Release the native operation/event args before filesystem cleanup;
            // they may retain handles on the staging directory after completion.
            entry.native.take();
            let id = entry.record.id;
            let filename = entry.record.filename.clone();
            let cancel = entry.cancel;
            let sender = self.sender.clone();
            let worker = std::thread::Builder::new()
                .name("download-finalize".into())
                .spawn(move || {
                    let result = if cancel {
                        cleanup(&staging).map(|()| None)
                    } else {
                        publish(&staging, &filename).map(Some)
                    };
                    sender.send(Event::Download(Signal::Finished(
                        id,
                        result.map_err(|e| format!("{e:#}")),
                    )));
                });
            match worker {
                Ok(_) => {
                    entry.worker = true;
                    entry.record.phase = Phase::Finalizing;
                }
                Err(error) => {
                    entry.record.phase = Phase::Failed;
                    entry.record.error = Some(error.to_string());
                    entry.native.take();
                }
            }
        }
        self.downloads.refresh();
    }
    pub(super) fn download_command(&mut self, op: Op) -> anyhow::Result<Value> {
        match op {
            Op::List {} => {}
            Op::Show {} => {
                self.download_ui(UiAction::Show)?;
            }
            Op::Cancel { id } => {
                let changed = self.download_cancel(id)?;
                self.downloads.refresh();
                return Ok(json!({"id":id,"changed":changed}));
            }
            Op::Remove { id } => {
                let index = self
                    .downloads
                    .entries
                    .iter()
                    .position(|e| e.record.id == id)
                    .context("download not found")?;
                anyhow::ensure!(
                    !self.downloads.entries[index].record.phase.active(),
                    "cancel or finish the download before removing it"
                );
                self.downloads.entries.remove(index);
            }
            Op::Clear {} => self.downloads.entries.retain(|e| e.record.phase.active()),
        }
        self.downloads.refresh();
        Ok(self.downloads.status())
    }
    pub(super) fn download_owner_closing(&mut self, owner: HWND) {
        if self
            .downloads
            .panel
            .as_ref()
            .is_some_and(|panel| panel.owner() == owner)
        {
            self.downloads.panel.take();
        }
    }
    pub(super) fn download_show_for(&mut self, id: SurfaceId) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.locate(id).is_some(),
            "download window source no longer exists"
        );
        let owner = self.surface_window(id);
        if self
            .downloads
            .panel
            .as_ref()
            .is_none_or(|panel| panel.owner() != owner)
        {
            let selected = self
                .downloads
                .panel
                .as_ref()
                .and_then(|panel| panel.selected());
            let panel = panel::Panel::new(owner)?;
            panel.results(&self.downloads.rows());
            panel.select(selected);
            self.downloads.panel = Some(panel);
        }
        self.downloads.refresh();
        self.downloads
            .panel
            .as_ref()
            .unwrap()
            .show(self.background_test);
        Ok(())
    }
    pub(super) fn download_ui(&mut self, action: UiAction) -> anyhow::Result<()> {
        let generation = match action {
            UiAction::Show => return self.download_show_for(self.active()),
            UiAction::Open(generation)
            | UiAction::Folder(generation)
            | UiAction::Cancel(generation)
            | UiAction::Remove(generation)
            | UiAction::Clear(generation)
            | UiAction::Close(generation)
            | UiAction::Selected(generation)
            | UiAction::Layout(generation) => generation,
        };
        let Some(panel) = &self.downloads.panel else {
            return Ok(());
        };
        if panel.generation != generation {
            return Ok(());
        }
        let id = panel.selected();
        match action {
            UiAction::Close(_) => panel.hide(),
            UiAction::Layout(_) => panel.layout(),
            UiAction::Selected(_) => panel.detail(),
            UiAction::Clear(_) => {
                self.download_command(Op::Clear {})?;
            }
            UiAction::Cancel(_) | UiAction::Remove(_) => {
                if let Some(id) = id {
                    self.download_command(if matches!(action, UiAction::Cancel(_)) {
                        Op::Cancel { id }
                    } else {
                        Op::Remove { id }
                    })?;
                }
            }
            UiAction::Open(_) | UiAction::Folder(_) => {
                anyhow::ensure!(
                    !self.background_test,
                    "opening downloads/folders is disabled in background hosts"
                );
                if let Some(entry) =
                    id.and_then(|id| self.downloads.entries.iter().find(|e| e.record.id == id))
                {
                    anyhow::ensure!(
                        entry.record.phase == Phase::Complete,
                        "download is not complete"
                    );
                    let path = entry
                        .record
                        .path
                        .as_ref()
                        .context("download path unavailable")?;
                    let target = if matches!(action, UiAction::Folder(_)) {
                        path.parent().context("download parent missing")?
                    } else {
                        path.as_path()
                    };
                    unsafe {
                        let result = ShellExecuteW(
                            panel.owner(),
                            wide("open").as_ptr(),
                            wide(target).as_ptr(),
                            std::ptr::null(),
                            std::ptr::null(),
                            SW_SHOWNORMAL,
                        );
                        anyhow::ensure!(
                            result as isize > 32,
                            "Windows could not open the download ({})",
                            result as isize
                        );
                    }
                }
            }
            UiAction::Show => unreachable!(),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_download_publication_never_replaces_existing_files() {
        let root = std::env::temp_dir().join(format!("flowmux-download-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let filename = "한 글 한 😀.txt";
        std::fs::write(root.join(filename), b"original").unwrap();
        let workers: Vec<_> = (0..4)
            .map(|n| {
                let root = root.clone();
                std::thread::spawn(move || {
                    let directory = root.join(n.to_string());
                    std::fs::create_dir(&directory).unwrap();
                    let path = directory.join(filename);
                    std::fs::write(&path, n.to_string()).unwrap();
                    publish(
                        &Staging {
                            root,
                            directory,
                            path,
                        },
                        filename,
                    )
                    .unwrap()
                })
            })
            .collect();
        let mut paths = std::collections::HashSet::new();
        for worker in workers {
            paths.insert(worker.join().unwrap());
        }
        assert_eq!(paths.len(), 4);
        assert_eq!(std::fs::read(root.join(filename)).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 5);
        std::fs::remove_dir_all(root).unwrap();
    }
}
