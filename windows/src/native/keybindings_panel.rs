// SPDX-License-Identifier: GPL-3.0-or-later
//! Native Keybindings page. Raw edit drafts are never normalized in place.
use super::*;
use crate::keybindings::{self, KeybindingOverrides, Op};
const SEARCH: usize = 600;
const LIST: usize = 601;
pub(super) const EDIT: usize = 602;
const SAVE: usize = 603;
const CLEAR: usize = 604;
const DEFAULT: usize = 605;
const RESET: usize = 606;
pub(super) const SAVE_INDEX: usize = usize::MAX - 1;

#[derive(Clone, Copy)]
pub(crate) enum Signal {
    Filter,
    Select,
    Changed,
    Save,
    Clear,
    Default,
    Reset,
}
pub(super) fn command(id: usize, code: u32) -> Option<Signal> {
    match (id, code) {
        (SEARCH, EN_CHANGE) if !SYNCING.with(Cell::get) => Some(Signal::Filter),
        (LIST, LBN_SELCHANGE) => Some(Signal::Select),
        (EDIT, EN_CHANGE) if !SYNCING.with(Cell::get) => Some(Signal::Changed),
        (SAVE, BN_CLICKED) => Some(Signal::Save),
        (CLEAR, BN_CLICKED) => Some(Signal::Clear),
        (DEFAULT, BN_CLICKED) => Some(Signal::Default),
        (RESET, BN_CLICKED) => Some(Signal::Reset),
        _ => None,
    }
}
struct Draft {
    action: String,
    label: String,
    supported: bool,
    reason: String,
    raw: String,
    baseline: Vec<String>,
    expected: KeybindingOverrides,
    error: Option<String>,
    dirty: bool,
}
#[derive(Clone, Copy)]
enum Kind {
    Set,
    Default,
    Reset,
}
struct Request {
    action: Option<usize>,
    raw: String,
    kind: Kind,
    expected: KeybindingOverrides,
    drafts: Vec<String>,
}
pub(super) struct Bindings {
    hint: HWND,
    search: HWND,
    list: HWND,
    title: HWND,
    input: HWND,
    detail: HWND,
    save: HWND,
    clear: HWND,
    defaults: HWND,
    reset: HWND,
    rows: Vec<Draft>,
    filtered: Vec<usize>,
    selected: Option<usize>,
    request: Option<Request>,
    pending: Option<Request>,
    baseline: KeybindingOverrides,
    error: Option<String>,
}
impl Bindings {
    pub(super) fn new(panel: &Panel) -> anyhow::Result<Self> {
        let hint = panel.child_in(panel.viewport, "STATIC", "Terminal input only; browser/editor/native shortcuts are not yet configurable. One shortcut per line: Ctrl+Shift+Y or <Ctrl><Shift>y. Save applies immediately; empty unbinds.", 610, SS_NOPREFIX)?;
        let search = panel.child_in(
            panel.viewport,
            "EDIT",
            "",
            SEARCH,
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
        )?;
        let list = panel.child_in(
            panel.viewport,
            "LISTBOX",
            "",
            LIST,
            WS_TABSTOP | WS_VSCROLL | LBS_NOTIFY as u32 | LBS_NOINTEGRALHEIGHT as u32,
        )?;
        let title = panel.child_in(
            panel.viewport,
            "STATIC",
            "Select an action",
            611,
            SS_NOPREFIX,
        )?;
        let input = panel.child_in(
            panel.viewport,
            "EDIT",
            "",
            EDIT,
            WS_TABSTOP
                | WS_BORDER
                | WS_VSCROLL
                | ES_MULTILINE as u32
                | ES_AUTOVSCROLL as u32
                | ES_WANTRETURN as u32,
        )?;
        let detail = panel.child_in(panel.viewport, "STATIC", "", 612, SS_NOPREFIX)?;
        let save = panel.child_in(panel.viewport, "BUTTON", "Save shortcut", SAVE, WS_TABSTOP)?;
        let clear = panel.child_in(panel.viewport, "BUTTON", "Unbind", CLEAR, WS_TABSTOP)?;
        let defaults =
            panel.child_in(panel.viewport, "BUTTON", "Use default", DEFAULT, WS_TABSTOP)?;
        let reset = panel.child_in(
            panel.viewport,
            "BUTTON",
            "Reset all keybindings",
            RESET,
            WS_TABSTOP,
        )?;
        unsafe {
            SendMessageW(search, EM_LIMITTEXT, 256, 0);
            SendMessageW(input, EM_LIMITTEXT, 2048, 0);
            SendMessageW(
                search,
                0x1501,
                1,
                wide("Search actions…").as_ptr() as LPARAM,
            );
            checked(SetWindowSubclass(input, Some(edit_proc), EDIT, 0))?;
            checked(SetWindowSubclass(search, Some(edit_proc), SEARCH, 0))?;
        }
        Ok(Self {
            hint,
            search,
            list,
            title,
            input,
            detail,
            save,
            clear,
            defaults,
            reset,
            rows: vec![],
            filtered: vec![],
            selected: None,
            request: None,
            pending: None,
            baseline: KeybindingOverrides::default(),
            error: None,
        })
    }
    pub(super) fn layout(&self, visible: bool, width: i32, height: i32, dpi: u32, offset: i32) {
        let px = |n: i32| n * dpi.max(96) as i32 / 96;
        let top = px(4);
        let bottom = height - px(12);
        let middle = px(8) + (width - px(28)) * 40 / 100;
        let right = width - middle - px(8);
        let placements = [
            (self.hint, px(8), top, width - px(16), px(56)),
            (self.search, px(8), top + px(60), width - px(16), px(30)),
            (
                self.list,
                px(8),
                top + px(100),
                middle - px(20),
                (bottom - top - px(144)).max(px(40)),
            ),
            (self.title, middle, top + px(100), right, px(44)),
            (
                self.input,
                middle,
                top + px(148),
                right,
                (bottom - top - px(264)).max(px(52)),
            ),
            (
                self.detail,
                middle,
                (bottom - px(110)).max(top + px(190)),
                right,
                px(72),
            ),
            (self.save, middle, bottom - px(32), px(112), px(28)),
            (
                self.clear,
                middle + px(118),
                bottom - px(32),
                px(66),
                px(28),
            ),
            (
                self.defaults,
                middle + px(190),
                bottom - px(32),
                (right - px(190)).max(px(68)),
                px(28),
            ),
            (self.reset, px(8), bottom - px(28), middle - px(20), px(28)),
        ];
        unsafe {
            for (window, x, y, w, h) in placements {
                SetWindowPos(
                    window,
                    std::ptr::null_mut(),
                    x,
                    y - offset,
                    w.max(1),
                    h.max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                ShowWindow(window, if visible { SW_SHOWNA } else { SW_HIDE });
            }
        }
    }
    pub(super) fn editing(&self, hwnd: HWND) -> bool {
        hwnd == self.input || hwnd == self.search
    }
    pub(super) fn has_draft(&self) -> bool {
        self.request.is_some()
            || self.pending.is_some()
            || self.rows.iter().any(|row| row.dirty || row.error.is_some())
    }
    pub(super) fn queued(&self) -> bool {
        self.request.is_some()
    }
    pub(super) fn enable(&self, idle: bool) {
        let supported = self
            .selected
            .is_some_and(|index| self.rows[index].supported);
        unsafe {
            EnableWindow(self.input, i32::from(supported));
            for control in [self.save, self.clear, self.defaults] {
                EnableWindow(control, i32::from(idle && supported));
            }
            EnableWindow(self.reset, i32::from(idle));
        }
    }
    fn store_draft(&mut self) {
        if let Some(index) = self.selected {
            if self.rows[index].supported {
                self.rows[index].raw = Panel::text(self.input);
                self.rows[index].dirty = true;
                self.rows[index].error = None;
            }
        }
    }
    fn capture_current(&mut self) {
        if self.selected.is_some_and(|index| {
            self.rows[index].supported && self.rows[index].raw != Panel::text(self.input)
        }) {
            self.store_draft();
        }
    }
    fn fill_selected(&self) {
        let Some(index) = self.selected else {
            SYNCING.with(|value| value.set(true));
            unsafe {
                SetWindowTextW(self.title, wide("No matching actions").as_ptr());
                SetWindowTextW(self.input, wide("").as_ptr());
                SetWindowTextW(
                    self.detail,
                    wide("Change the search to select an action.").as_ptr(),
                );
            }
            SYNCING.with(|value| value.set(false));
            return;
        };
        let row = &self.rows[index];
        SYNCING.with(|value| value.set(true));
        unsafe {
            SetWindowTextW(
                self.title,
                wide(format!("{}\n{}", row.label, row.action)).as_ptr(),
            );
            if Panel::text(self.input) != row.raw {
                SetWindowTextW(self.input, wide(&row.raw).as_ptr());
            }
            SetWindowTextW(
                self.detail,
                wide(row.error.as_deref().unwrap_or(&row.reason)).as_ptr(),
            );
        }
        SYNCING.with(|value| value.set(false));
    }
    fn filter(&mut self) {
        let query = Panel::text(self.search).to_lowercase();
        self.filtered = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                query.is_empty()
                    || row.label.to_lowercase().contains(&query)
                    || row.action.contains(&query)
            })
            .map(|(index, _)| index)
            .collect();
        if !self
            .selected
            .is_some_and(|index| self.filtered.contains(&index))
        {
            self.selected = self.filtered.first().copied();
        }
        unsafe {
            SendMessageW(self.list, WM_SETREDRAW, 0, 0);
            SendMessageW(self.list, LB_RESETCONTENT, 0, 0);
            for index in &self.filtered {
                let row = &self.rows[*index];
                let label = format!(
                    "{}{}",
                    row.label,
                    if row.supported { "" } else { " (not editable)" }
                );
                SendMessageW(self.list, LB_ADDSTRING, 0, wide(label).as_ptr() as LPARAM);
            }
            if let Some(position) = self.selected.and_then(|index| {
                self.filtered
                    .iter()
                    .position(|candidate| *candidate == index)
            }) {
                SendMessageW(self.list, LB_SETCURSEL, position, 0);
            }
            SendMessageW(self.list, WM_SETREDRAW, 1, 0);
            InvalidateRect(self.list, std::ptr::null(), 1);
        }
    }
    pub(super) fn reload(&mut self, document: &crate::settings::Document) {
        self.request = None;
        self.pending = None;
        self.baseline = document.keybindings.clone();
        self.error = None;
        match keybindings::catalog(&document.keybindings) {
            Ok(catalog) => {
                self.rows=catalog.into_iter().map(|item| Draft {
                    reason: if item.supported {
                        "Applies to terminal input. Browser, editor and native controls keep their current shortcuts.".into()
                    } else if matches!(item.action.as_str(), "copy" | "paste") {
                        "Clipboard shortcuts are fixed to preserve copy, paste and terminal Ctrl+C behavior; they cannot be rebound here.".into()
                    } else { "This action has no configurable Windows terminal shortcut; no binding is dispatched here.".into() },
                    action:item.action,label:item.label,supported:item.supported,
                    raw:item.accels.join("\r\n"),baseline:item.accels,expected:document.keybindings.clone(),error:None,dirty:false,
                }).collect();
                self.selected = (!self.rows.is_empty()).then_some(0);
                self.filter();
                self.fill_selected();
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    pub(super) fn signal(&mut self, signal: Signal, idle: bool) {
        if matches!(signal, Signal::Changed) {
            self.store_draft();
            return;
        }
        if COMPOSING.with(Cell::get) != 0 {
            return;
        }
        match signal {
            Signal::Filter => {
                self.capture_current();
                self.filter();
                self.fill_selected();
            }
            Signal::Select => {
                self.capture_current();
                let position = unsafe { SendMessageW(self.list, LB_GETCURSEL, 0, 0) };
                if let Some(index) = self.filtered.get(position as usize).copied() {
                    self.selected = Some(index);
                    self.fill_selected();
                }
            }
            Signal::Save | Signal::Clear | Signal::Default if idle => {
                let Some(index) = self.selected.filter(|index| self.rows[*index].supported) else {
                    return;
                };
                self.store_draft();
                if matches!(signal, Signal::Clear) {
                    self.rows[index].raw.clear();
                    self.fill_selected();
                }
                self.rows[index].error = None;
                self.error = None;
                self.request = Some(Request {
                    action: Some(index),
                    raw: self.rows[index].raw.clone(),
                    kind: if matches!(signal, Signal::Default) {
                        Kind::Default
                    } else {
                        Kind::Set
                    },
                    expected: self.rows[index].expected.clone(),
                    drafts: Vec::new(),
                });
            }
            Signal::Reset if idle => {
                self.capture_current();
                self.error = None;
                self.request = Some(Request {
                    action: None,
                    raw: String::new(),
                    kind: Kind::Reset,
                    expected: self.baseline.clone(),
                    drafts: self.rows.iter().map(|row| row.raw.clone()).collect(),
                });
            }
            _ => {}
        }
    }
    pub(super) fn operation(
        &self,
        document: &crate::settings::Document,
    ) -> anyhow::Result<SettingsOp> {
        let request = self
            .request
            .as_ref()
            .context("No keybinding change queued")?;
        let expected = Some(request.expected.clone());
        let op = match request.kind {
            Kind::Reset => Op::Reset(keybindings::ResetArgs { expected }),
            Kind::Default => Op::Clear(keybindings::ClearArgs {
                action: self.rows[request.action.unwrap()].action.clone(),
                expected,
            }),
            Kind::Set => Op::Set(keybindings::SetArgs {
                action: self.rows[request.action.unwrap()].action.clone(),
                accels: request
                    .raw
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect(),
                expected,
            }),
        };
        keybindings::change(&document.keybindings, &op)?;
        Ok(SettingsOp::Keybindings { op })
    }
    pub(super) fn begin(&mut self) {
        self.pending = self.request.take();
    }
    pub(super) fn failed(&mut self, error: &str) {
        let request = self.request.take();
        if let Some(index) = request.and_then(|request| request.action) {
            self.rows[index].error = Some(error.into());
        }
        self.error = Some(error.into());
        self.fill_selected();
    }
    pub(super) fn update(
        &mut self,
        document: &crate::settings::Document,
        error: Option<&str>,
        completed: bool,
    ) {
        // Native EN_CHANGE posts a queued event. Read the live EDIT before an
        // earlier worker reply can overwrite keystrokes not yet drained by App.
        self.capture_current();
        let pending = if completed { self.pending.take() } else { None };
        self.baseline = document.keybindings.clone();
        let Ok(catalog) = keybindings::catalog(&document.keybindings) else {
            return;
        };
        for (index, row) in self.rows.iter_mut().enumerate() {
            let Some(item) = catalog.iter().find(|item| item.action == row.action) else {
                continue;
            };
            let own = pending.as_ref().is_some_and(|request| {
                request.action == Some(index) || matches!(request.kind, Kind::Reset)
            });
            let composing =
                self.selected == Some(index) && COMPOSING.with(Cell::get) == self.input as isize;
            if own && error.is_none() {
                let unchanged = pending.as_ref().is_some_and(|request| {
                    if matches!(request.kind, Kind::Reset) {
                        request.drafts.get(index) == Some(&row.raw)
                    } else {
                        row.raw == request.raw
                    }
                });
                if unchanged && !composing {
                    row.raw = item.accels.join("\r\n");
                    row.dirty = false;
                }
                row.baseline = item.accels.clone();
                row.expected = document.keybindings.clone();
                row.error = None;
            } else if !row.dirty && !composing {
                row.raw = item.accels.join("\r\n");
                row.baseline = item.accels.clone();
                row.expected = document.keybindings.clone();
            } else if error.is_none()
                && pending
                    .as_ref()
                    .is_some_and(|request| request.expected == row.expected)
            {
                // Rebase other drafts only over our acknowledged CAS write, not external changes.
                row.expected = document.keybindings.clone();
            }
            if own && error.is_some() {
                row.error = error.map(str::to_owned);
            }
        }
        if completed {
            self.error = error.map(str::to_owned);
        }
        // Updating other fields must never replace an active native IME buffer.
        if COMPOSING.with(Cell::get) != self.input as isize {
            self.fill_selected();
        }
    }
    pub(super) fn diagnostics(&self) -> Value {
        json!({"scope":"terminal_input","auto_apply":false,"parent":unsafe{GetParent(self.input)} as usize,
            "search":self.search as usize,"list":self.list as usize,"input":self.input as usize,"save":self.save as usize,"unbind":self.clear as usize,"default":self.defaults as usize,"reset":self.reset as usize,
            "selected":self.selected.map(|index|&self.rows[index].action),"draft":Panel::text(self.input),"pending":self.pending.is_some(),"queued":self.request.is_some(),"error":self.error,
            "visible_actions":self.filtered.iter().map(|index|&self.rows[*index].action).collect::<Vec<_>>(),
            "actions":self.rows.iter().map(|row|json!({"action":row.action,"label":row.label,"supported":row.supported,"accels":row.baseline,"draft":row.raw,"dirty":row.dirty,"error":row.error,"reason":row.reason})).collect::<Vec<_>>()})
    }
}
