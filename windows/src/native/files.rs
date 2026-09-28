// SPDX-License-Identifier: GPL-3.0-or-later
//! Window-wide Files dock with bounded source-pane state. Filesystem work belongs to files_service.
use super::*;
use crate::{files_model as domain, files_service as worker};
use std::collections::HashSet;
use windows_sys::Win32::UI::Controls::EM_SETLIMITTEXT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetKeyState, VK_CONTROL};

#[path = "files_actions.rs"]
mod actions;

pub(super) const TIMER: usize = 4;
const MAX_PANES: usize = 32;
const MAX_CACHE: usize = 16 * 1024 * 1024;
const LIST_ID: usize = 20;

pub(super) enum Signal {
    Worker(worker::Response),
    Operation(crate::files_operations::Event),
    Ui(UiRequest),
    OpenFinished(domain::Owner, Value),
    Tick,
}
#[derive(Clone, Copy)]
enum FormKind {
    Copy,
    Rename,
    Move,
}
impl FormKind {
    fn title(self) -> &'static str {
        match self {
            Self::Copy => "Copy to",
            Self::Rename => "Rename to",
            Self::Move => "Move to",
        }
    }
}
#[derive(Clone)]
struct Form {
    kind: FormKind,
    token: Uuid,
    index: usize,
    label: String,
}
#[derive(Clone)]
enum Action {
    Menu(i32, i32),
    Begin(FormKind),
    DismissForm,
    Refresh,
    More,
    Open,
    Expand,
    Collapse,
    Hide,
    Copy(String),
    Rename(String),
    Move(String),
    CancelOperation(Uuid),
    Select {
        indices: Vec<usize>,
        caret: Option<usize>,
        anchor: Option<usize>,
        retain_hidden: bool,
    },
}
#[derive(Clone)]
pub(super) struct UiRequest {
    owner: domain::Owner,
    token: Option<Uuid>,
    index: Option<usize>,
    action: Action,
}
#[derive(Clone)]
struct Binding {
    owner: domain::Owner,
    token: Option<Uuid>,
    indices: Vec<usize>,
    list: isize,
    destination: isize,
    form: Option<Form>,
}
thread_local! {
    static ACTIVE_OPERATION: std::cell::Cell<Option<Uuid>> = const { std::cell::Cell::new(None) };
    static BINDINGS: RefCell<HashMap<isize, Binding>> = RefCell::new(HashMap::new());
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(result) = chrome::message(window, message, wparam, lparam) {
        return result;
    }
    match message {
        WM_CONTEXTMENU => {
            let binding = BINDINGS.with(|map| map.borrow().get(&(window as isize)).cloned());
            if let Some(binding) = binding {
                let at = SendMessageW(binding.list as HWND, LB_GETCARETINDEX, 0, 0);
                let mut index = usize::try_from(at)
                    .ok()
                    .and_then(|at| binding.indices.get(at).copied());
                let mut x = lparam as u16 as i16 as i32;
                let mut y = (lparam >> 16) as u16 as i16 as i32;
                if x >= 0 && y >= 0 && wparam as isize == binding.list {
                    let mut point = POINT { x, y };
                    ScreenToClient(binding.list as HWND, &mut point);
                    let hit = SendMessageW(
                        binding.list as HWND,
                        LB_ITEMFROMPOINT,
                        0,
                        ((point.y as u16 as usize) << 16 | point.x as u16 as usize) as isize,
                    );
                    index = if (hit as usize >> 16) == 0 {
                        binding.indices.get(hit as u16 as usize).copied()
                    } else {
                        None
                    };
                }
                if x == -1 && y == -1 {
                    let mut r = RECT::default();
                    GetWindowRect(binding.list as HWND, &mut r);
                    x = r.left;
                    y = r.top;
                }
                post(Event::Files(Signal::Ui(UiRequest {
                    owner: binding.owner,
                    token: binding.token,
                    index,
                    action: Action::Menu(x, y),
                })));
            }
            0
        }
        WM_COMMAND => {
            let binding = BINDINGS.with(|map| map.borrow().get(&(window as isize)).cloned());
            let Some(binding) = binding else { return 0 };
            let list = binding.list as HWND;
            let native_index = SendMessageW(list, LB_GETCARETINDEX, 0, 0);
            let mut index = usize::try_from(native_index)
                .ok()
                .and_then(|index| binding.indices.get(index).copied());
            let mut token = binding.token;
            let code = (wparam >> 16) as u32;
            let destination = || {
                let edit = binding.destination as HWND;
                let length = GetWindowTextLengthW(edit).clamp(0, 16384) as usize;
                let mut text = vec![0u16; length + 1];
                let read = GetWindowTextW(edit, text.as_mut_ptr(), text.len() as i32);
                String::from_utf16(&text[..read.max(0) as usize]).ok()
            };
            let action = match (wparam & 0xffff, code) {
                (13, BN_CLICKED) => {
                    let mut r = RECT::default();
                    GetWindowRect(lparam as HWND, &mut r);
                    Some(Action::Menu(r.left, r.bottom))
                }
                (14, BN_CLICKED) => binding.form.as_ref().and_then(|form| {
                    index = Some(form.index);
                    token = Some(form.token);
                    destination().map(|text| match form.kind {
                        FormKind::Copy => Action::Copy(text),
                        FormKind::Rename => Action::Rename(text),
                        FormKind::Move => Action::Move(text),
                    })
                }),
                (15, BN_CLICKED) => Some(Action::DismissForm),
                (1, BN_CLICKED) => Some(Action::Refresh),
                (2, BN_CLICKED) => Some(Action::More),
                (3, BN_CLICKED) | (LIST_ID, LBN_DBLCLK) => Some(Action::Open),
                (4, BN_CLICKED) => Some(Action::Expand),
                (5, BN_CLICKED) => Some(Action::Collapse),
                (6, BN_CLICKED) => Some(Action::Hide),
                (7, BN_CLICKED) => Some(Action::Begin(FormKind::Copy)),
                (8, BN_CLICKED) => Some(Action::Begin(FormKind::Rename)),
                (9, BN_CLICKED) => Some(Action::Begin(FormKind::Move)),
                (10, BN_CLICKED) => ACTIVE_OPERATION
                    .with(|id| id.get())
                    .map(Action::CancelOperation),
                (LIST_ID, LBN_SELCHANGE) => {
                    let count = SendMessageW(list, LB_GETSELCOUNT, 0, 0);
                    if count < 0 || count as usize > binding.indices.len() {
                        return 0;
                    }
                    let mut native = vec![0i32; count as usize];
                    let read = SendMessageW(
                        list,
                        LB_GETSELITEMS,
                        native.len(),
                        native.as_mut_ptr() as isize,
                    );
                    if read < 0 || read as usize > native.len() {
                        return 0;
                    }
                    let indices = native
                        .into_iter()
                        .take(read as usize)
                        .filter_map(|index| {
                            usize::try_from(index)
                                .ok()
                                .and_then(|index| binding.indices.get(index).copied())
                        })
                        .collect();
                    let anchor = SendMessageW(list, LB_GETANCHORINDEX, 0, 0);
                    Some(Action::Select {
                        indices,
                        caret: index,
                        anchor: usize::try_from(anchor)
                            .ok()
                            .and_then(|index| binding.indices.get(index).copied()),
                        retain_hidden: GetKeyState(VK_CONTROL as i32) < 0,
                    })
                }
                _ => None,
            };
            if let Some(action) = action {
                post(Event::Files(Signal::Ui(UiRequest {
                    owner: binding.owner,
                    token,
                    index,
                    action,
                })));
            }
            0
        }
        WM_NCDESTROY => {
            BINDINGS.with(|map| {
                map.borrow_mut().remove(&(window as isize));
            });
            DefWindowProcW(window, message, wparam, lparam)
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

struct Panel {
    window: HWND,
    list: HWND,
    heading: HWND,
    path: HWND,
    form_label: HWND,
    destination: HWND,
    status: HWND,
    buttons: Vec<HWND>,
    indices: Vec<usize>,
    token: Option<Uuid>,
    shown: bool,
    form: Option<Form>,
    more: bool,
    area: Option<model::Rect>,
    scale: f64,
    background: bool,
    list_top: i32,
}
impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
impl Panel {
    fn new(parent: HWND) -> anyhow::Result<Self> {
        unsafe {
            let class = wide("flowmux.windows.files");
            let instance = GetModuleHandleW(std::ptr::null());
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: (COLOR_BTNFACE + 1) as HBRUSH,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register Files controls"
            );
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Files").as_ptr(),
                WS_CHILD | WS_CLIPCHILDREN,
                0,
                0,
                1,
                1,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create Files panel");
            chrome::register_control(window, chrome::ControlRole::Static);
            let mut panel = Self {
                window,
                list: std::ptr::null_mut(),
                heading: std::ptr::null_mut(),
                path: std::ptr::null_mut(),
                form_label: std::ptr::null_mut(),
                destination: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                buttons: Vec::new(),
                indices: Vec::new(),
                token: None,
                shown: false,
                form: None,
                more: false,
                area: None,
                scale: 1.0,
                background: true,
                list_top: 58,
            };
            panel.heading = panel.child("STATIC", "Files", 16, 0)?;
            panel.path = panel.child("STATIC", "", 17, 0)?;
            panel.form_label = panel.child("STATIC", "", 18, 0)?;
            panel.status = panel.child("STATIC", "", 11, 0)?;
            panel.destination = panel.child(
                "EDIT",
                "",
                12,
                WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            )?;
            SendMessageW(panel.destination, EM_SETLIMITTEXT, 16384, 0);
            SendMessageW(
                panel.destination,
                0x1501,
                1,
                wide("Path relative to Files root / new name").as_ptr() as isize,
            );
            panel.list = panel.child(
                "LISTBOX",
                "",
                LIST_ID,
                WS_BORDER
                    | WS_TABSTOP
                    | WS_VSCROLL
                    | WS_HSCROLL
                    | LBS_EXTENDEDSEL as u32
                    | LBS_NOTIFY as u32
                    | LBS_NOINTEGRALHEIGHT as u32,
            )?;
            for (id, title) in [
                (1, "Refresh"),
                (2, "More"),
                (3, "Open"),
                (4, "Expand"),
                (5, "Collapse"),
                (6, "Hide"),
                (7, "Copy to"),
                (8, "Rename to"),
                (9, "Move to"),
                (10, "Cancel job"),
                (13, "Actions"),
                (14, "Apply"),
                (15, "Cancel"),
            ] {
                panel
                    .buttons
                    .push(panel.child("BUTTON", title, id, WS_TABSTOP)?);
            }
            Ok(panel)
        }
    }
    fn child(&self, class: &str, text: &str, id: usize, style: u32) -> anyhow::Result<HWND> {
        unsafe {
            let window = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD
                    | WS_VISIBLE
                    | style
                    | if class == "BUTTON" {
                        BS_OWNERDRAW as u32
                    } else {
                        0
                    },
                0,
                0,
                1,
                1,
                self.window,
                id as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create Files control");
            match class {
                "BUTTON" => chrome::register_button(window, chrome::Role::Button),
                "EDIT" => chrome::register_control(window, chrome::ControlRole::Edit),
                "LISTBOX" => chrome::register_control(window, chrome::ControlRole::Listbox),
                _ => chrome::register_control(window, chrome::ControlRole::Caption),
            }
            Ok(window)
        }
    }
    fn layout(&mut self, area: Option<model::Rect>, scale: f64, background: bool) {
        self.area = area;
        self.scale = scale;
        self.background = background;
        let show = area.is_some() && !background;
        unsafe {
            if self.shown != show {
                ShowWindow(self.window, if show { SW_SHOWNA } else { SW_HIDE });
                self.shown = show;
            }
            let Some(area) = area else { return };
            let px = |n: i32| (n as f64 * scale).round() as i32;
            SetWindowPos(
                self.window,
                std::ptr::null_mut(),
                area.x,
                area.y,
                area.width.max(1),
                area.height.max(1),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let margin = px(6);
            let width = (area.width - margin * 2).max(1);
            let operation = ACTIVE_OPERATION.with(|id| id.get().is_some());
            let form = self.form.is_some();
            self.list_top = px(if form { 140 } else { 58 });
            let status_y = (area.height - px(26)).max(self.list_top);
            let more_y = status_y - if self.more || operation { px(28) } else { 0 };
            let position = |window: HWND, rect: Option<(i32, i32, i32, i32)>| {
                if let Some((x, y, w, h)) = rect.filter(|(x, y, w, h)| {
                    *x >= 0
                        && *y >= 0
                        && *w > 0
                        && *h > 0
                        && x + w <= area.width
                        && y + h <= area.height
                }) {
                    SetWindowPos(
                        window,
                        std::ptr::null_mut(),
                        x,
                        y,
                        w,
                        h,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                } else {
                    ShowWindow(window, SW_HIDE);
                }
            };
            for (at, button) in self.buttons.iter().enumerate() {
                let rect = match at {
                    5 => Some((area.width - px(46), px(4), px(40), px(26))),
                    10 => Some((area.width - px(112), px(4), px(62), px(26))),
                    1 if self.more => Some((margin, more_y, (width / 2 - px(2)).max(1), px(24))),
                    9 if operation => {
                        Some((margin + width / 2, more_y, (width / 2).max(1), px(24)))
                    }
                    11 if form => Some((margin, px(108), (width / 2 - px(2)).max(1), px(26))),
                    12 if form => Some((margin + width / 2, px(108), (width / 2).max(1), px(26))),
                    _ => None,
                };
                position(*button, rect);
            }
            position(
                self.heading,
                Some((margin, px(5), (width - px(112)).max(1), px(22))),
            );
            position(self.path, Some((margin, px(32), width, px(20))));
            position(
                self.form_label,
                form.then_some((margin, px(58), width, px(20))),
            );
            position(
                self.destination,
                form.then_some((margin, px(80), width, px(24))),
            );
            position(self.status, Some((margin, status_y, width, px(22))));
            position(
                self.list,
                Some((
                    margin,
                    self.list_top,
                    width,
                    (more_y - self.list_top - px(4)).max(0),
                )),
            );
            SendMessageW(
                self.list,
                LB_SETHORIZONTALEXTENT,
                px(2400).max(1) as usize,
                0,
            );
        }
    }
    fn render(&mut self, state: &PaneState) -> anyhow::Result<()> {
        let rows = state.model.rendered_rows();
        let indices: Vec<_> = rows.iter().map(|row| row.index).collect();
        let changed = self.token != state.model.token() || self.indices != indices;
        if self
            .form
            .as_ref()
            .is_some_and(|form| Some(form.token) != state.model.token())
        {
            self.form = None;
        }
        unsafe {
            let top = SendMessageW(self.list, LB_GETTOPINDEX, 0, 0).max(0);
            if changed {
                SendMessageW(self.list, WM_SETREDRAW, 0, 0);
                SendMessageW(self.list, LB_RESETCONTENT, 0, 0);
                for row in &rows {
                    let prefix = if row.row.reparse {
                        "[!] "
                    } else if row.row.directory {
                        if row.row.expanded {
                            "[-] "
                        } else {
                            "[+] "
                        }
                    } else {
                        "    "
                    };
                    let text = format!(
                        "{}{}{}",
                        "  ".repeat(row.row.depth as usize),
                        prefix,
                        row.row.name
                    );
                    let added =
                        SendMessageW(self.list, LB_ADDSTRING, 0, wide(text).as_ptr() as isize);
                    if added < 0 {
                        SendMessageW(self.list, WM_SETREDRAW, 1, 0);
                        anyhow::bail!("cannot render Files row");
                    }
                }
                self.indices = indices;
                self.token = state.model.token();
            }
            SendMessageW(self.list, LB_SETSEL, 0, -1);
            for (at, row) in rows.iter().enumerate() {
                if row.selected {
                    SendMessageW(self.list, LB_SETSEL, 1, at as isize);
                }
                if row.caret {
                    SendMessageW(self.list, LB_SETCARETINDEX, at, 0);
                }
            }
            if let Some(anchor) = state.model.anchor_index().and_then(|index| {
                self.indices
                    .iter()
                    .position(|candidate| *candidate == index)
            }) {
                SendMessageW(self.list, LB_SETANCHORINDEX, anchor, 0);
            }
            SendMessageW(
                self.list,
                LB_SETTOPINDEX,
                (top as usize).min(rows.len().saturating_sub(1)),
                0,
            );
            if changed {
                SendMessageW(self.list, WM_SETREDRAW, 1, 0);
                InvalidateRect(self.list, std::ptr::null(), 1);
            }
            SetWindowTextW(self.path, wide(state.root.display().to_string()).as_ptr());
            let page = state.model.page(0)?;
            self.more = page.more_available;
            if let Some(form) = &self.form {
                SetWindowTextW(
                    self.form_label,
                    wide(format!("{}: {}", form.kind.title(), form.label)).as_ptr(),
                );
            }
            let text = if state.pending.is_some() {
                "Loading files…".to_owned()
            } else if let Some(error) = state.message.as_ref().or(page.error.as_ref()) {
                error.clone()
            } else {
                format!(
                    "{} shown · {} selected{}",
                    page.rendered_rows,
                    page.selected_count,
                    if page.truncated || !page.warnings.is_empty() {
                        " · incomplete listing"
                    } else {
                        ""
                    }
                )
            };
            SetWindowTextW(self.status, wide(text).as_ptr());
            for (at, button) in self.buttons.iter().enumerate() {
                let enabled = at == 10
                    || at == 12
                    || at == 9
                    || at == 0
                    || at == 5
                    || (state.pending.is_none() && !page.stale && (at != 1 || page.more_available));
                EnableWindow(*button, i32::from(enabled));
            }
        }
        BINDINGS.with(|map| {
            map.borrow_mut().insert(
                self.window as isize,
                Binding {
                    owner: state.owner.clone(),
                    token: state.model.token(),
                    indices: self.indices.clone(),
                    list: self.list as isize,
                    destination: self.destination as isize,
                    form: self.form.clone(),
                },
            );
        });
        self.layout(self.area, self.scale, self.background);
        Ok(())
    }
    fn native_count(&self) -> usize {
        unsafe { SendMessageW(self.list, LB_GETCOUNT, 0, 0).max(0) as usize }
    }
    fn selected_indices(&self) -> Vec<usize> {
        unsafe {
            let count = SendMessageW(self.list, LB_GETSELCOUNT, 0, 0).max(0) as usize;
            let mut values = vec![0i32; count.min(self.indices.len())];
            let read = SendMessageW(
                self.list,
                LB_GETSELITEMS,
                values.len(),
                values.as_mut_ptr() as isize,
            )
            .max(0) as usize;
            values
                .into_iter()
                .take(read)
                .filter_map(|index| usize::try_from(index).ok())
                .collect()
        }
    }
}

struct Pending {
    owner: domain::Owner,
    deadline: Instant,
    ticket: worker::Ticket,
    reply: Option<ipc::Reply>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.ticket.cancel();
    }
}
struct PaneState {
    owner: domain::Owner,
    source: SurfaceId,
    root: PathBuf,
    model: domain::Model,
    pending: Option<Pending>,
    panel: Option<Panel>,
    visible: bool,
    touched: u64,
    message: Option<String>,
}
impl PaneState {
    fn bytes(&self) -> usize {
        Self::model_bytes(&self.model, &self.root)
    }
    fn model_bytes(model: &domain::Model, root: &std::path::Path) -> usize {
        // A conservative ceiling includes owned model strings, native UTF-16
        // labels, cached indices and row/hash metadata, rather than just paths.
        model
            .cache_bytes()
            .saturating_mul(3)
            .saturating_add(model.snapshot_rows().saturating_mul(256))
            .saturating_add(root.as_os_str().len().saturating_mul(4))
    }
    fn render(&mut self) -> anyhow::Result<()> {
        if let Some(mut panel) = self.panel.take() {
            let result = panel.render(self);
            self.panel = Some(panel);
            result?;
        }
        Ok(())
    }
}
#[derive(Default)]
pub(super) struct Controller {
    active: Option<PaneId>,
    states: HashMap<PaneId, PaneState>,
    service: Option<worker::Service>,
    clock: u64,
    actions: actions::State,
}
impl Controller {
    fn cancel(&mut self, pane: PaneId, reason: &str) {
        if let Some(state) = self.states.get_mut(&pane) {
            if let Some(pending) = state.pending.take() {
                if let Some(reply) = &pending.reply {
                    let _ = reply.try_send(json!({"error":reason}));
                }
            }
            if let Some(service) = &self.service {
                service.cancel_owner(state.owner.instance);
            }
        }
    }
    fn remove(&mut self, pane: PaneId, reason: &str) {
        self.cancel(pane, reason);
        self.states.remove(&pane);
        if self.active == Some(pane) {
            self.active = None;
        }
    }
    fn capacity(&mut self, pane: PaneId, bytes: usize) -> anyhow::Result<()> {
        anyhow::ensure!(bytes <= MAX_CACHE, "Files pane exceeds the cache limit");
        loop {
            let entries = self.states.len() + usize::from(!self.states.contains_key(&pane));
            let total = bytes
                + self
                    .states
                    .iter()
                    .filter(|(id, _)| **id != pane)
                    .map(|(_, state)| state.bytes())
                    .sum::<usize>();
            if entries <= MAX_PANES && total <= MAX_CACHE {
                return Ok(());
            }
            let evict = self
                .states
                .iter()
                .filter(|(id, state)| **id != pane && !state.visible && state.pending.is_none())
                .min_by_key(|(id, state)| (state.touched, id.0))
                .map(|(id, _)| *id)
                .context("Files cache is full; hide another Files panel first")?;
            self.remove(evict, "Files cache entry was evicted");
        }
    }
}

impl App {
    fn files_activate(&mut self, pane: PaneId) {
        if let Some(previous) = self.files.active.filter(|previous| *previous != pane) {
            if let Some(state) = self.files.states.get(&previous) {
                self.editor_cancel_files_opens(state.owner.instance, "Files source changed");
            }
            self.files.cancel(previous, "Files source changed");
            if let Some(state) = self.files.states.get_mut(&previous) {
                state.visible = false;
                if let Some(panel) = &mut state.panel {
                    panel.form = None;
                }
            }
        }
        self.files.active = Some(pane);
        if let Some(state) = self.files.states.get_mut(&pane) {
            state.visible = true;
        }
        self.files_operation_tick();
    }
    pub(super) fn files_dock_width(
        &self,
        workspace: usize,
        workbench_width: i32,
        scale: f64,
    ) -> i32 {
        let visible = self
            .files
            .active
            .and_then(|pane| self.files.states.get(&pane))
            .is_some_and(|state| {
                state.visible && state.owner.workspace == self.workspaces[workspace].id.0
            });
        if visible {
            ((320.0 * scale).round() as i32)
                .min((workbench_width - (160.0 * scale).round() as i32).max(0))
        } else {
            0
        }
    }
    pub(super) fn files_command(
        &mut self,
        op: domain::Op,
        reply: ipc::Reply,
    ) -> anyhow::Result<Option<Value>> {
        self.files_dispatch(op, Some(reply))
    }
    pub(super) fn files_show_current(&mut self) -> anyhow::Result<()> {
        let pane = self.workspace().focused;
        if self.files.active == Some(pane)
            && self
                .files
                .states
                .get(&pane)
                .is_some_and(|state| state.visible)
        {
            self.files_dispatch(domain::Op::Hide(domain::PaneArgs { pane: pane.0 }), None)?;
            return Ok(());
        }
        self.files_dispatch(
            domain::Op::Show(domain::ShowArgs {
                pane: self.workspace().focused.0,
                root: None,
            }),
            None,
        )?;
        Ok(())
    }
    fn files_dispatch(
        &mut self,
        op: domain::Op,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<Option<Value>> {
        if matches!(
            &op,
            domain::Op::Copy(_)
                | domain::Op::Move(_)
                | domain::Op::Rename(_)
                | domain::Op::OperationStatus(_)
                | domain::Op::OperationCancel(_)
        ) {
            let changes_controls = !matches!(&op, domain::Op::OperationStatus(_));
            let result = self.files_operation_command(op, reply)?;
            if changes_controls {
                self.layout()?;
            }
            return Ok(result);
        }
        if matches!(&op, domain::Op::Show(_)) {
            anyhow::ensure!(
                reply
                    .as_ref()
                    .is_none_or(|reply| reply.received_at().elapsed() < worker::BUDGET),
                "Files Show expired before admission; prior panel was preserved"
            );
        }
        self.files_reconcile();
        let pane = PaneId(match &op {
            domain::Op::Show(args) => args.pane,
            domain::Op::Status(args) => args.pane,
            domain::Op::Expand(args) | domain::Op::Collapse(args) | domain::Op::Open(args) => {
                args.pane
            }
            domain::Op::Select(args) => args.pane,
            domain::Op::More(args) => args.pane,
            domain::Op::Refresh(args) | domain::Op::Hide(args) => args.pane,
            _ => unreachable!(),
        });
        if let domain::Op::Status(args) = op {
            return Ok(Some(self.files_status(pane, args.offset)?));
        }
        anyhow::ensure!(
            !self.closing && !self.close_accepted && self.close_request.is_none(),
            "Files is unavailable while the window closes"
        );
        if let domain::Op::Show(args) = op {
            let reuse = self.files.states.get(&pane).is_some_and(|state| {
                args.root
                    .as_ref()
                    .is_none_or(|root| crate::editor_search::same_path(root, &state.root))
            });
            if !reuse {
                let source = self.target(Some(pane.0), None)?;
                let (index, actual_pane, cwd) =
                    self.locate(source).context("Files source disappeared")?;
                anyhow::ensure!(actual_pane == pane, "Files source pane changed");
                let root = args
                    .root
                    .map(|root| {
                        if root.is_absolute() {
                            root
                        } else {
                            cwd.join(root)
                        }
                    })
                    .unwrap_or_else(|| {
                        self.workspaces[index]
                            .leaves()
                            .into_iter()
                            .flat_map(|(_, _, tabs)| tabs)
                            .find(|tab| tab.id == source)
                            .and_then(|tab| match tab.kind {
                                SurfaceKind::Editor { workspace_root, .. } => Some(workspace_root),
                                _ => None,
                            })
                            .unwrap_or(cwd)
                    });
                crate::editor::validate_path(&root)?;
                anyhow::ensure!(
                    root.is_absolute(),
                    "Files root must be an absolute local path"
                );
                let mut state = PaneState {
                    owner: domain::Owner {
                        workspace: self.workspaces[index].id.0,
                        pane: pane.0,
                        instance: Uuid::new_v4(),
                        generation: 1,
                    },
                    source,
                    root,
                    model: domain::Model::default(),
                    pending: None,
                    panel: None,
                    visible: true,
                    touched: self.files.clock,
                    message: None,
                };
                let pending =
                    self.files_submit(state.owner.clone(), state.root.clone(), Vec::new(), reply)?;
                state.pending = Some(pending);
                self.files.capacity(pane, state.bytes())?;
                state.panel = Some(Panel::new(self.window)?);
                state.render()?;
                anyhow::ensure!(
                    state
                        .pending
                        .as_ref()
                        .is_some_and(|pending| Instant::now() < pending.deadline),
                    "Files Show expired before replacing the previous panel"
                );
                if let Some(previous) = self.files.states.get(&pane) {
                    self.editor_cancel_files_opens(
                        previous.owner.instance,
                        "Files root was replaced",
                    );
                }
                self.files.remove(pane, "Files root was replaced");
                self.files.states.insert(pane, state);
                self.files_activate(pane);
                self.files_schedule();
                self.layout()?;
                return Ok(None);
            }
            self.files_scan(pane, None, reply)?;
            self.files_activate(pane);
            self.layout()?;
            return Ok(None);
        }
        anyhow::ensure!(
            self.files.states.contains_key(&pane),
            "Files has not been shown for this pane"
        );
        self.files.clock = self.files.clock.saturating_add(1);
        self.files.states.get_mut(&pane).unwrap().touched = self.files.clock;
        let expand = matches!(&op, domain::Op::Expand(_));
        match op {
            domain::Op::Refresh(_) => {
                anyhow::ensure!(
                    self.files.states[&pane].visible,
                    "Files is hidden; show it before refreshing"
                );
                self.files_scan(pane, None, reply)?;
                return Ok(None);
            }
            domain::Op::Hide(_) => {
                let instance = self.files.states[&pane].owner.instance;
                self.editor_cancel_files_opens(instance, "Files panel was hidden");
                self.files.cancel(pane, "Files panel was hidden");
                if self.files.active == Some(pane) {
                    self.files.active = None;
                }
                let state = self.files.states.get_mut(&pane).unwrap();
                state.visible = false;
                state.owner.generation = state
                    .owner
                    .generation
                    .checked_add(1)
                    .context("Files generation exhausted")?;
                state.model.fail("Files is hidden; show it before acting");
                state.message = None;
                state.render()?;
                self.files_schedule();
                self.layout()?;
            }
            domain::Op::Open(args) => {
                let state = &self.files.states[&pane];
                anyhow::ensure!(
                    state.visible && state.pending.is_none(),
                    "Files is hidden or loading"
                );
                let path = state.model.open_path(args.token, args.index)?;
                self.editor_open_from_files(
                    state.source,
                    state.owner.clone(),
                    state.root.clone(),
                    path,
                    reply,
                )?;
                return Ok(None);
            }
            domain::Op::Expand(args) | domain::Op::Collapse(args) => {
                let state = &self.files.states[&pane];
                anyhow::ensure!(
                    state.visible && state.pending.is_none(),
                    "Files is hidden or loading"
                );
                let mut candidate = state.model.clone();
                if candidate.set_expanded(args.token, args.index, expand)? {
                    self.files_scan(pane, Some(candidate), reply)?;
                    return Ok(None);
                }
                self.files_commit_model(pane, candidate)?;
            }
            domain::Op::Select(args) => {
                let state = &self.files.states[&pane];
                anyhow::ensure!(
                    state.visible && state.pending.is_none(),
                    "Files is hidden or loading"
                );
                let mut candidate = state.model.clone();
                candidate.select(args.token, args.index, args.mode)?;
                self.files_commit_model(pane, candidate)?;
            }
            domain::Op::More(args) => {
                let state = &self.files.states[&pane];
                anyhow::ensure!(
                    state.visible && state.pending.is_none(),
                    "Files is hidden or loading"
                );
                let mut candidate = state.model.clone();
                candidate.more(args.token)?;
                self.files_commit_model(pane, candidate)?;
            }
            _ => unreachable!(),
        }
        self.files.states.get_mut(&pane).unwrap().message = None;
        self.files.states.get_mut(&pane).unwrap().render()?;
        Ok(Some(self.files_status(pane, 0)?))
    }
    fn files_commit_model(&mut self, pane: PaneId, candidate: domain::Model) -> anyhow::Result<()> {
        let bytes = PaneState::model_bytes(&candidate, &self.files.states[&pane].root);
        self.files.capacity(pane, bytes)?;
        self.files.states.get_mut(&pane).unwrap().model = candidate;
        Ok(())
    }
    fn files_submit(
        &mut self,
        owner: domain::Owner,
        root: PathBuf,
        expanded: Vec<String>,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<Pending> {
        let deadline = reply
            .as_ref()
            .map_or_else(Instant::now, ipc::Reply::received_at)
            + worker::BUDGET;
        anyhow::ensure!(
            Instant::now() < deadline,
            "Files request expired before admission"
        );
        if self.files.service.is_none() {
            let sender = self.sender.clone();
            self.files.service = Some(worker::Service::start(move |response| {
                sender.send(Event::Files(Signal::Worker(response)))
            })?);
        }
        let ticket = self
            .files
            .service
            .as_ref()
            .unwrap()
            .submit(worker::Request {
                owner: owner.clone(),
                root,
                expanded,
                deadline,
            })?;
        Ok(Pending {
            owner,
            deadline,
            ticket,
            reply,
        })
    }
    fn files_scan(
        &mut self,
        pane: PaneId,
        candidate: Option<domain::Model>,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<()> {
        let state = &self.files.states[&pane];
        if let Some(candidate) = &candidate {
            let bytes = PaneState::model_bytes(candidate, &state.root);
            self.files.capacity(pane, bytes)?;
        }
        let state = &self.files.states[&pane];
        let mut owner = state.owner.clone();
        owner.generation = owner
            .generation
            .checked_add(1)
            .context("Files generation exhausted")?;
        let expanded = candidate.as_ref().unwrap_or(&state.model).refresh_inputs();
        let pending = self.files_submit(owner.clone(), state.root.clone(), expanded, reply)?;
        // submit supersedes old jobs. Cancel only the old ticket here: cancelling
        // the entire instance would also cancel this newly admitted request.
        let state = self.files.states.get_mut(&pane).unwrap();
        if let Some(previous) = state.pending.take() {
            if let Some(reply) = &previous.reply {
                let _ = reply.try_send(json!({"error":"Files request was superseded"}));
            }
        }
        state.owner = owner.clone();
        if let Some(candidate) = candidate {
            state.model = candidate;
        }
        state.pending = Some(pending);
        state.message = None;
        if let Err(error) = state.render() {
            self.files
                .cancel(pane, "Files controls could not render the pending request");
            self.files.states.get_mut(&pane).unwrap().model.fail(&error);
            self.files_schedule();
            return Err(error);
        }
        self.files_schedule();
        Ok(())
    }
    pub(super) fn files_event(&mut self, signal: Signal) -> anyhow::Result<()> {
        match signal {
            Signal::Tick => self.files_tick(),
            Signal::Operation(event) => {
                self.files_operation_event(event)?;
                self.layout()?;
            }
            Signal::Worker(mut response) => {
                self.files_reconcile();
                let pane = PaneId(response.owner.pane);
                let Some(state) = self.files.states.get(&pane).filter(|state| {
                    state.owner == response.owner
                        && state.visible
                        && state
                            .pending
                            .as_ref()
                            .is_some_and(|pending| pending.owner == response.owner)
                }) else {
                    return Ok(());
                };
                let expired = state.pending.as_ref().is_some_and(|pending| {
                    Instant::now() >= pending.deadline || pending.ticket.is_cancelled()
                });
                let mut error = response.error.take();
                if expired {
                    error = Some(
                        "Files result arrived after cancellation or its four-second deadline"
                            .into(),
                    );
                }
                let snapshot = response.snapshot.take();
                if error.is_none() {
                    if let Some(snapshot) = snapshot {
                        if snapshot.owner != response.owner
                            || !crate::editor_search::same_path(&snapshot.root, &state.root)
                        {
                            error = Some(
                                "Files result does not match the captured root and owner".into(),
                            );
                        } else {
                            let mut candidate = state.model.clone();
                            match candidate.apply(snapshot) {
                                Ok(_) => {
                                    if let Err(failure) = self.files_commit_model(pane, candidate) {
                                        error = Some(failure.to_string());
                                    }
                                }
                                Err(failure) => error = Some(failure.to_string()),
                            }
                        }
                    } else {
                        error = Some("Files worker returned no directory snapshot".into());
                    }
                }
                let state = self.files.states.get_mut(&pane).unwrap();
                let pending = state.pending.take().unwrap();
                if let Some(error) = &error {
                    state.model.fail(error);
                    state.message = Some(domain::short_error(error));
                }
                if let Err(failure) = state.render() {
                    error = Some(failure.to_string());
                    state.model.fail(&failure);
                }
                if error.is_none() && Instant::now() >= pending.deadline {
                    let reason = "Files result exceeded its original deadline while rendering; refresh before acting";
                    error = Some(reason.into());
                    state.model.fail(reason);
                    state.message = Some(reason.into());
                    let _ = state.render();
                }
                // Keep Response (and its worker admission permit) until the
                // complete snapshot was consumed, then expose settled service state.
                drop(response);
                if let Some(reply) = &pending.reply {
                    let value = if let Some(error) = error {
                        json!({"error":error})
                    } else {
                        self.files_status(pane, 0)
                            .unwrap_or_else(|error| json!({"error":error.to_string()}))
                    };
                    let _ = reply.try_send(value);
                }
                self.files_schedule();
            }
            Signal::OpenFinished(owner, value) => {
                if let Some(state) = self
                    .files
                    .states
                    .get_mut(&PaneId(owner.pane))
                    .filter(|state| state.owner == owner && state.visible)
                {
                    state.message = value
                        .get("error")
                        .and_then(Value::as_str)
                        .map(domain::short_error);
                    state.render()?;
                }
            }
            Signal::Ui(request) => self.files_ui(request),
        }
        Ok(())
    }
    fn files_ui(&mut self, request: UiRequest) {
        let pane = PaneId(request.owner.pane);
        let cancelling = matches!(&request.action, Action::CancelOperation(_));
        if self.files.states.get(&pane).is_none_or(|state| {
            (!cancelling && (state.owner != request.owner || state.model.token() != request.token))
                || !state.visible
        }) {
            return;
        }
        let action = (|| -> anyhow::Result<()> {
            let token = request.token;
            let row = || -> anyhow::Result<domain::RowArgs> {
                Ok(domain::RowArgs {
                    pane: pane.0,
                    token: token.context("Files is still loading")?,
                    index: request.index.context("Select a file or directory first")?,
                })
            };
            let op = match request.action {
                Action::Menu(x, y) => {
                    let labels = [
                        "Open",
                        "Expand",
                        "Collapse",
                        "Copy to…",
                        "Rename…",
                        "Move to…",
                        "Refresh",
                        "Load more",
                        "Hide Files",
                    ];
                    let selected = request.index.is_some() && request.token.is_some();
                    let disabled = if selected {
                        Vec::new()
                    } else {
                        vec![1, 2, 3, 4, 5, 6]
                    };
                    let choice = self.popup(&labels, &disabled, (x, y))?;
                    let action = match choice {
                        1 => Action::Open,
                        2 => Action::Expand,
                        3 => Action::Collapse,
                        4 => Action::Begin(FormKind::Copy),
                        5 => Action::Begin(FormKind::Rename),
                        6 => Action::Begin(FormKind::Move),
                        7 => Action::Refresh,
                        8 => Action::More,
                        9 => Action::Hide,
                        _ => {
                            if !self.background_test {
                                if let Some(panel) = self.files.states[&pane]
                                    .panel
                                    .as_ref()
                                    .filter(|panel| panel.shown)
                                {
                                    unsafe {
                                        SetFocus(panel.list);
                                    }
                                }
                            }
                            return Ok(());
                        }
                    };
                    self.files_ui(UiRequest {
                        owner: request.owner.clone(),
                        token: request.token,
                        index: request.index,
                        action,
                    });
                    return Ok(());
                }
                Action::Begin(kind) => {
                    let row = row()?;
                    let state = self.files.states.get_mut(&pane).unwrap();
                    anyhow::ensure!(state.pending.is_none(), "Files is loading");
                    state.model.open_path(row.token, row.index)?;
                    let label = state
                        .model
                        .rendered_rows()
                        .into_iter()
                        .find(|row| row.index == request.index.unwrap())
                        .context("Selected Files row disappeared")?
                        .row
                        .name;
                    if let Some(panel) = &mut state.panel {
                        panel.form = Some(Form {
                            kind,
                            token: row.token,
                            index: row.index,
                            label,
                        });
                        unsafe {
                            SetWindowTextW(panel.destination, wide("").as_ptr());
                        }
                    }
                    state.render()?;
                    if !self.background_test {
                        if let Some(panel) = state.panel.as_ref().filter(|panel| panel.shown) {
                            unsafe {
                                SetFocus(panel.destination);
                            }
                        }
                    }
                    return Ok(());
                }
                Action::DismissForm => {
                    let state = self.files.states.get_mut(&pane).unwrap();
                    if let Some(panel) = &mut state.panel {
                        panel.form = None;
                    }
                    state.render()?;
                    return Ok(());
                }
                Action::Refresh => domain::Op::Refresh(domain::PaneArgs { pane: pane.0 }),
                Action::More => domain::Op::More(domain::TokenArgs {
                    pane: pane.0,
                    token: token.context("Files is still loading")?,
                }),
                Action::Open => {
                    let args = row()?;
                    let directory = self.files.states[&pane]
                        .model
                        .rendered_rows()
                        .iter()
                        .any(|row| row.index == args.index && row.row.directory);
                    if directory {
                        domain::Op::Expand(args)
                    } else {
                        domain::Op::Open(args)
                    }
                }
                Action::CancelOperation(id) => {
                    domain::Op::OperationCancel(domain::OperationArgs { id })
                }
                Action::Copy(destination) => {
                    let row = row()?;
                    domain::Op::Copy(domain::ActionArgs {
                        pane: row.pane,
                        token: row.token,
                        index: row.index,
                        destination,
                    })
                }
                Action::Move(destination) => {
                    let row = row()?;
                    domain::Op::Move(domain::ActionArgs {
                        pane: row.pane,
                        token: row.token,
                        index: row.index,
                        destination,
                    })
                }
                Action::Rename(name) => {
                    let row = row()?;
                    domain::Op::Rename(domain::RenameArgs {
                        pane: row.pane,
                        token: row.token,
                        index: row.index,
                        name,
                    })
                }
                Action::Expand => domain::Op::Expand(row()?),
                Action::Collapse => domain::Op::Collapse(row()?),
                Action::Hide => domain::Op::Hide(domain::PaneArgs { pane: pane.0 }),
                Action::Select {
                    indices,
                    caret,
                    anchor,
                    retain_hidden,
                } => {
                    let state = &self.files.states[&pane];
                    anyhow::ensure!(state.pending.is_none(), "Files is loading");
                    let mut candidate = state.model.clone();
                    candidate.select_native(
                        token.context("Files is still loading")?,
                        &indices,
                        caret,
                        anchor,
                        retain_hidden,
                    )?;
                    self.files_commit_model(pane, candidate)?;
                    let state = self.files.states.get_mut(&pane).unwrap();
                    state.message = None;
                    state.render()?;
                    return Ok(());
                }
            };
            self.files_dispatch(op, None)?;
            self.layout()?;
            Ok(())
        })();
        if let Err(error) = action {
            if let Some(state) = self.files.states.get_mut(&pane) {
                state.message = Some(domain::short_error(error));
                let _ = state.render();
            }
        }
    }
    pub(super) fn files_status(&self, pane: PaneId, offset: usize) -> anyhow::Result<Value> {
        let state = self
            .files
            .states
            .get(&pane)
            .context("Files has not been shown for this pane")?;
        let mut value = serde_json::to_value(state.model.page(offset)?)?;
        let error = value
            .as_object_mut()
            .unwrap()
            .remove("error")
            .unwrap_or(Value::Null);
        value["last_error"] = state.message.as_ref().map_or(error, |error| json!(error));
        value["pane"] = json!(pane);
        value["source"] = json!(state.source);
        value["visible"] = json!(state.visible);
        value["dock_source_pane"] = json!(self.files.active);
        value["dock_visible"] = json!(
            self.files.active == Some(pane)
                && state.visible
                && state.owner.workspace == self.workspace().id.0
        );
        value["dock_scope"] = json!("window");
        value["dock_bounds"] = json!(state
            .panel
            .as_ref()
            .and_then(|panel| panel.area)
            .map(|r| json!({"x":r.x,"y":r.y,"width":r.width,"height":r.height})));
        value["list_top"] = json!(state.panel.as_ref().map(|panel| panel.list_top));
        value["operation_form"] = json!(state.panel.as_ref().and_then(|panel|panel.form.as_ref()).map(|form|json!({"kind":form.kind.title(),"token":form.token,"index":form.index,"source_label":form.label})));
        value["loading"] = json!(state.pending.is_some());
        value["current_owner"] = json!(state.owner);
        value["captured_root"] = json!(state.root);
        value["panel_handle"] = json!(state.panel.as_ref().map(|panel| panel.window as usize));
        value["destination_handle"] =
            json!(state.panel.as_ref().map(|panel| panel.destination as usize));
        value["operation"] = self.files_operation_current();
        value["list_handle"] = json!(state.panel.as_ref().map(|panel| panel.list as usize));
        value["native_count"] = json!(state.panel.as_ref().map_or(0, Panel::native_count));
        value["native_selected_indices"] = json!(state
            .panel
            .as_ref()
            .map_or_else(Vec::new, Panel::selected_indices));
        value["cache_bytes"] = json!(self
            .files
            .states
            .values()
            .map(PaneState::bytes)
            .sum::<usize>());
        value["cached_panes"] = json!(self.files.states.len());
        value["service"] =
            serde_json::to_value(self.files.service.as_ref().map(worker::Service::status))?;
        let selected = state.model.selected_paths();
        let visible: HashSet<_> = state
            .model
            .rendered_rows()
            .into_iter()
            .filter(|row| row.selected)
            .map(|row| row.row.path)
            .collect();
        for (key, paths) in [
            ("selected_paths", selected.clone()),
            (
                "visible_selected_paths",
                selected
                    .into_iter()
                    .filter(|path| visible.contains(path))
                    .collect(),
            ),
        ] {
            let total = paths.len();
            let mut bytes = 2;
            let mut retained = Vec::new();
            for path in paths.into_iter().take(domain::PAGE_SIZE) {
                let size = serde_json::to_vec(&path)?.len() + 1;
                if bytes + size > 16 * 1024 {
                    break;
                }
                bytes += size;
                retained.push(path);
            }
            value[format!("{key}_truncated")] = json!(retained.len() < total);
            value[key] = json!(retained);
        }
        anyhow::ensure!(
            serde_json::to_vec(&value)?.len() <= domain::MAX_PAGE_BYTES,
            "Files status exceeds its byte limit"
        );
        Ok(value)
    }
    pub(super) fn files_reconcile(&mut self) {
        let remove: Vec<_> = self
            .files
            .states
            .iter()
            .filter(|(pane, state)| {
                self.closing
                    || self.close_accepted
                    || self
                        .locate(state.source)
                        .is_none_or(|(workspace, current, _)| {
                            current != **pane
                                || self.workspaces[workspace].id.0 != state.owner.workspace
                        })
            })
            .map(|(pane, _)| *pane)
            .collect();
        for pane in remove {
            let instance = self.files.states[&pane].owner.instance;
            self.editor_cancel_files_opens(instance, "Files source closed or moved");
            self.files.remove(pane, "Files source closed or moved");
        }
    }
    pub(super) fn files_owner_current(&self, owner: &domain::Owner) -> bool {
        self.files
            .states
            .get(&PaneId(owner.pane))
            .is_some_and(|state| {
                state.owner == *owner
                    && state.visible
                    && !self.closing
                    && !self.close_accepted
                    && self.close_request.is_none()
                    && self.locate(state.source).is_some_and(|(index, pane, _)| {
                        pane.0 == owner.pane && self.workspaces[index].id.0 == owner.workspace
                    })
            })
    }
    pub(super) fn files_tick(&mut self) {
        self.files_operation_tick();
        self.files_reconcile();
        let expired: Vec<_> = self
            .files
            .states
            .iter()
            .filter(|(_, state)| {
                state
                    .pending
                    .as_ref()
                    .is_some_and(|pending| Instant::now() >= pending.deadline)
            })
            .map(|(pane, _)| *pane)
            .collect();
        for pane in expired {
            self.files.cancel(
                pane,
                "Files request exceeded its original four-second deadline; not retried",
            );
            if let Some(state) = self.files.states.get_mut(&pane) {
                state
                    .model
                    .fail("Files listing timed out; refresh to try again");
                let _ = state.render();
            }
        }
        self.files_schedule();
    }
    fn files_schedule(&mut self) {
        unsafe {
            KillTimer(self.window, TIMER);
        }
        let next = self
            .files
            .states
            .values()
            .filter_map(|state| state.pending.as_ref().map(|pending| pending.deadline))
            .chain(self.files_operation_next_tick())
            .min();
        if let Some(next) = next {
            let millis = next
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(1, 100) as u32;
            if unsafe { SetTimer(self.window, TIMER, millis, None) } == 0 {
                let panes: Vec<_> = self.files.states.keys().copied().collect();
                for pane in panes {
                    self.files
                        .cancel(pane, "Files deadline timer is unavailable");
                    if let Some(state) = self.files.states.get_mut(&pane) {
                        state.model.fail("Files deadline timer is unavailable");
                        let _ = state.render();
                    }
                }
            }
        }
    }
    pub(super) fn files_layout(
        &mut self,
        area: Option<model::Rect>,
        scale: f64,
    ) -> anyhow::Result<()> {
        self.files_reconcile();
        let workspace = self.workspace().id.0;
        for (pane, state) in &mut self.files.states {
            let visible = self.files.active == Some(*pane)
                && state.visible
                && state.owner.workspace == workspace;
            if let Some(panel) = &mut state.panel {
                panel.layout(area.filter(|_| visible), scale, self.background_test);
            }
        }
        Ok(())
    }
    pub(super) fn files_handle_message(&self, message: &MSG) -> bool {
        if self.background_test {
            return false;
        }
        self.files
            .states
            .values()
            .filter_map(|state| state.panel.as_ref())
            .any(|panel| unsafe {
                panel.shown
                    && (message.hwnd == panel.window || IsChild(panel.window, message.hwnd) != 0)
                    && !(message.hwnd == panel.destination
                        && matches!(message.message, WM_KEYDOWN | WM_KEYUP | WM_CHAR)
                        && matches!(message.wParam, 13 | 27))
                    && IsDialogMessageW(panel.window, message) != 0
            })
    }
    pub(super) fn files_shutdown(&mut self) {
        unsafe {
            KillTimer(self.window, TIMER);
        }
        let panes: Vec<_> = self.files.states.keys().copied().collect();
        for pane in panes {
            let instance = self.files.states[&pane].owner.instance;
            self.editor_cancel_files_opens(instance, "Files host is shutting down");
            self.files.remove(pane, "Files host is shutting down");
        }
        if let Some(service) = self.files.service.take() {
            service.stop();
        }
    }
}
