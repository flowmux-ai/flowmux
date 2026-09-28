// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux-style action rows with a transactional, owned shortcut editor.
use super::*;
use crate::keybindings::{self, KeybindingOverrides, Op};
#[path = "keybindings_editor.rs"]
mod editor;
const RESET_ALL: usize = 606;
const ROW_BASE: usize = 1000;
pub(super) const SAVE_INDEX: usize = usize::MAX - 1;

#[derive(Clone, Copy)]
pub(crate) enum Signal {
    Edit(usize),
    Editor(Uuid, editor::Signal),
    ResetAll,
    Reveal(usize),
}
fn emit(signal: Signal) {
    super::emit(UiAction::Bindings(signal));
}
pub(super) fn command(id: usize, code: u32) -> Option<Signal> {
    match (id, code) {
        (RESET_ALL, BN_CLICKED) => Some(Signal::ResetAll),
        (id, BN_CLICKED) if (ROW_BASE..ROW_BASE + 37).contains(&id) => {
            Some(Signal::Edit(id - ROW_BASE))
        }
        (id, BN_SETFOCUS) if (ROW_BASE..ROW_BASE + 37).contains(&id) => {
            Some(Signal::Reveal(id - ROW_BASE))
        }
        _ => None,
    }
}
struct Row {
    action: String,
    label: String,
    supported: bool,
    defaults: Vec<String>,
    accels: Vec<String>,
    title: HWND,
    subtitle: HWND,
    accel: HWND,
    edit: HWND,
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
    editor: Option<Uuid>,
}
pub(super) struct Bindings {
    owner: HWND,
    viewport: HWND,
    hint: HWND,
    reset: HWND,
    rows: Vec<Row>,
    selected: Option<usize>,
    editor: Option<editor::Panel>,
    editor_expected: KeybindingOverrides,
    request: Option<Request>,
    pending: Option<Request>,
    baseline: KeybindingOverrides,
    error: Option<String>,
    background: bool,
    theme: crate::settings::Theme,
}
impl Bindings {
    pub(super) fn new(panel: &Panel) -> anyhow::Result<Self> {
        let hint = panel.child_in(panel.viewport, "STATIC", "Changes take effect after OK. Terminal input only; browser/editor/native shortcuts are not configurable. Clipboard Copy/Paste keep fixed defaults. Unavailable actions are shown read-only.", 610, SS_NOPREFIX)?;
        let reset = panel.child(
            "BUTTON",
            "Reset all keybindings to defaults",
            RESET_ALL,
            WS_TABSTOP,
        )?;
        let mut rows = Vec::new();
        for item in keybindings::catalog(&KeybindingOverrides::default())?
            .into_iter()
            .filter(|item| !matches!(item.action.as_str(), "copy" | "paste"))
        {
            let index = rows.len();
            let title = panel.child_in(
                panel.viewport,
                "STATIC",
                &item.label,
                1100 + index,
                SS_NOPREFIX,
            )?;
            let subtitle = panel.child_in(
                panel.viewport,
                "STATIC",
                &item.action,
                1200 + index,
                SS_NOPREFIX,
            )?;
            let accel = panel.child_in(panel.viewport, "STATIC", "", 1300 + index, SS_NOPREFIX)?;
            let edit = panel.child_in(
                panel.viewport,
                "BUTTON",
                "Edit",
                ROW_BASE + index,
                WS_TABSTOP | BS_NOTIFY as u32,
            )?;
            chrome::register_control(subtitle, chrome::ControlRole::Caption);
            chrome::register_control(accel, chrome::ControlRole::Caption);
            rows.push(Row {
                action: item.action,
                label: item.label,
                supported: item.supported,
                defaults: item.defaults,
                accels: item.accels,
                title,
                subtitle,
                accel,
                edit,
            });
        }
        let page = Self {
            owner: panel.window,
            viewport: panel.viewport,
            hint,
            reset,
            rows,
            selected: None,
            editor: None,
            editor_expected: KeybindingOverrides::default(),
            request: None,
            pending: None,
            baseline: KeybindingOverrides::default(),
            error: None,
            background: true,
            theme: crate::settings::Theme::Dark,
        };
        page.render_rows();
        Ok(page)
    }
    pub(super) fn environment(&mut self, background: bool, theme: crate::settings::Theme) {
        self.background = background;
        self.theme = theme;
        if let Some(editor) = self.editor.as_ref() {
            editor.theme(theme);
        }
    }
    pub(super) fn content_height(&self, dpi: u32) -> i32 {
        (80 + self.rows.len() as i32 * 68) * dpi.max(96) as i32 / 96
    }
    pub(super) fn layout(
        &self,
        visible: bool,
        width: i32,
        dpi: u32,
        offset: i32,
        owner_height: i32,
    ) {
        let px = |n: i32| n * dpi.max(96) as i32 / 96;
        unsafe {
            let place = |window: HWND, x: i32, y: i32, w: i32, h: i32| {
                SetWindowPos(
                    window,
                    std::ptr::null_mut(),
                    x,
                    y,
                    w.max(1),
                    h.max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                ShowWindow(window, if visible { SW_SHOWNA } else { SW_HIDE });
            };
            place(self.hint, px(8), px(4) - offset, width - px(16), px(64));
            for (index, row) in self.rows.iter().enumerate() {
                let y = px(80 + index as i32 * 68) - offset;
                let left = (width - px(110)) * 48 / 100;
                let chip_x = left + px(12);
                place(row.title, px(8), y, left - px(12), px(28));
                place(row.subtitle, px(8), y + px(28), left - px(12), px(22));
                place(
                    row.accel,
                    chip_x,
                    y + px(8),
                    width - chip_x - px(94),
                    px(42),
                );
                place(row.edit, width - px(82), y + px(10), px(74), px(30));
            }
            place(self.reset, px(20), owner_height - px(40), px(270), px(28));
        }
    }
    fn render_rows(&self) {
        unsafe {
            for row in &self.rows {
                let text = if row.accels.is_empty() {
                    "(unbound)".to_owned()
                } else {
                    row.accels.join(", ")
                };
                SetWindowTextW(row.accel, wide(text).as_ptr());
                SetWindowTextW(
                    row.subtitle,
                    wide(if row.supported {
                        row.action.clone()
                    } else {
                        format!("{} · shortcut unavailable", row.action)
                    })
                    .as_ptr(),
                );
            }
        }
    }
    pub(super) fn reveal_delta(&self, index: usize) -> i32 {
        let Some(row) = self.rows.get(index) else {
            return 0;
        };
        unsafe {
            let mut rect = RECT::default();
            let mut viewport = RECT::default();
            GetWindowRect(row.edit, &mut rect);
            GetWindowRect(self.viewport, &mut viewport);
            if rect.top < viewport.top {
                rect.top - viewport.top - 8
            } else if rect.bottom > viewport.bottom {
                rect.bottom - viewport.bottom + 8
            } else {
                0
            }
        }
    }
    pub(super) fn focus(&self) {
        if let Some(editor) = self.editor.as_ref() {
            editor.focus();
        }
    }
    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub(super) fn modal(&self) -> bool {
        self.editor.is_some()
    }
    pub(super) fn has_draft(&self) -> bool {
        self.editor.is_some() || self.request.is_some() || self.pending.is_some()
    }
    pub(super) fn queued(&self) -> bool {
        self.request.is_some()
    }
    pub(super) fn enable(&self, idle: bool) {
        unsafe {
            for row in &self.rows {
                EnableWindow(
                    row.edit,
                    i32::from(
                        idle && row.supported && self.editor.is_none() && self.request.is_none(),
                    ),
                );
            }
            EnableWindow(self.reset, i32::from(idle && self.editor.is_none()));
        }
    }
    pub(super) fn reload(&mut self, document: &crate::settings::Document) {
        if self.editor.is_some() {
            return;
        }
        self.request = None;
        self.pending = None;
        self.error = None;
        self.baseline = document.keybindings.clone();
        if let Ok(catalog) = keybindings::catalog(&document.keybindings) {
            for row in &mut self.rows {
                if let Some(item) = catalog.iter().find(|item| item.action == row.action) {
                    row.accels = item.accels.clone();
                }
            }
        }
        self.render_rows();
    }
    pub(super) fn signal(&mut self, signal: Signal, idle: bool) {
        match signal {
            Signal::Edit(index) if idle && self.request.is_none() => {
                if let Some(editor) = self.editor.as_ref() {
                    editor.focus();
                    return;
                }
                let Some(row) = self.rows.get(index).filter(|row| row.supported) else {
                    return;
                };
                match editor::Panel::new(
                    self.owner,
                    &row.label,
                    &row.accels.join(", "),
                    self.background,
                ) {
                    Ok(editor) => {
                        editor.theme(self.theme);
                        self.editor_expected = self.baseline.clone();
                        self.selected = Some(index);
                        self.error = None;
                        self.editor = Some(editor);
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            Signal::Editor(id, signal) => {
                let Some(dialog) = self.editor.as_mut().filter(|editor| editor.id == id) else {
                    return;
                };
                match signal {
                    editor::Signal::Cancel => {
                        if self
                            .request
                            .as_ref()
                            .is_some_and(|request| request.editor == Some(id))
                        {
                            self.request = None;
                        }
                        self.editor.take();
                        self.selected = None;
                        self.error = None;
                    }
                    editor::Signal::Changed => {
                        self.error = None;
                        dialog.status("");
                    }
                    editor::Signal::Layout => dialog.layout(),
                    editor::Signal::CaptureCancel => dialog.capture_close(),
                    editor::Signal::Capture
                        if idle && self.request.is_none() && !dialog.composing() =>
                    {
                        if let Err(error) = dialog.capture_start() {
                            dialog.status(&error.to_string());
                        }
                    }
                    editor::Signal::Reset | editor::Signal::Unbind
                        if idle && self.request.is_none() && !dialog.composing() =>
                    {
                        if let Some(index) = self.selected {
                            dialog.set_raw(&if matches!(signal, editor::Signal::Reset) {
                                self.rows[index].defaults.join(", ")
                            } else {
                                String::new()
                            });
                            dialog.status("");
                            self.error = None;
                        }
                    }
                    editor::Signal::Confirm
                        if self.pending.is_none()
                            && self.request.is_none()
                            && !dialog.composing() =>
                    {
                        if let Some(index) = self.selected {
                            let raw = dialog.raw();
                            let values = split_accels(&raw);
                            let kind = if values == self.rows[index].defaults {
                                Kind::Default
                            } else {
                                Kind::Set
                            };
                            self.request = Some(Request {
                                action: Some(index),
                                raw,
                                kind,
                                expected: self.editor_expected.clone(),
                                editor: Some(id),
                            });
                            dialog.pending(true);
                        }
                    }
                    _ => {}
                }
            }
            Signal::ResetAll if idle && self.editor.is_none() && self.request.is_none() => {
                self.request = Some(Request {
                    action: None,
                    raw: String::new(),
                    kind: Kind::Reset,
                    expected: self.baseline.clone(),
                    editor: None,
                });
                self.error = None;
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
                accels: split_accels(&request.raw),
                expected,
            }),
        };
        keybindings::change(&document.keybindings, &op)?;
        Ok(SettingsOp::Keybindings { op })
    }
    pub(super) fn begin(&mut self) {
        self.pending = self.request.take();
        if let Some(editor) = self.editor.as_mut() {
            editor.pending(true);
        }
    }
    pub(super) fn failed(&mut self, error: &str) {
        self.request = None;
        self.error = Some(error.into());
        if let Some(editor) = self.editor.as_mut() {
            editor.status(error);
            editor.pending(false);
        }
    }
    pub(super) fn update(
        &mut self,
        document: &crate::settings::Document,
        error: Option<&str>,
        completed: bool,
    ) {
        let pending = if completed { self.pending.take() } else { None };
        self.baseline = document.keybindings.clone();
        self.theme = document.terminal.theme;
        if let Ok(catalog) = keybindings::catalog(&document.keybindings) {
            for row in &mut self.rows {
                if let Some(item) = catalog.iter().find(|item| item.action == row.action) {
                    row.accels = item.accels.clone();
                }
            }
            self.render_rows();
        }
        let mut close = false;
        if let Some(dialog) = self.editor.as_mut() {
            dialog.theme(self.theme);
            if let Some(request) = pending
                .as_ref()
                .filter(|request| request.editor == Some(dialog.id))
            {
                dialog.pending(false);
                if let Some(error) = error {
                    dialog.status(error);
                    self.error = Some(error.into());
                } else {
                    self.editor_expected = document.keybindings.clone();
                    close = dialog.raw() == request.raw && !dialog.composing();
                    if !close {
                        dialog.status("Saved. Newer draft remains unsaved; press OK to apply it.");
                    }
                    self.error = None;
                }
            }
        }
        if close {
            self.editor.take();
            self.selected = None;
        }
        if completed && error.is_some() {
            self.error = error.map(str::to_owned);
        }
    }
    pub(super) fn dismiss(&mut self) {
        if self
            .request
            .as_ref()
            .is_some_and(|request| request.editor.is_some())
        {
            self.request = None;
        }
        self.editor.take();
        self.selected = None;
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|editor| editor.handle_message(message))
    }
    #[cfg(debug_assertions)]
    pub(super) fn capture_window(&self) -> Option<HWND> {
        self.editor.as_ref().map(editor::Panel::capture_window)
    }
    pub(super) fn diagnostics(&self) -> Value {
        let mut editor = self.editor.as_ref().map(editor::Panel::diagnostics);
        if let Some(Value::Object(value)) = &mut editor {
            value.insert(
                "action".into(),
                json!(self.selected.map(|index| &self.rows[index].action)),
            );
        }
        json!({"scope":"terminal_input","auto_apply":false,"parent":self.viewport as usize,"reset":self.reset as usize,
            "reset_parent":self.owner as usize,"editor":editor,"pending":self.pending.is_some(),"queued":self.request.is_some(),"error":self.error,
            "fixed_clipboard":["copy","paste"],"actions":self.rows.iter().map(|row|json!({"action":row.action,"label":row.label,"supported":row.supported,"accels":row.accels,"edit":row.edit as usize,"accel_handle":row.accel as usize,"chip_text":if row.accels.is_empty(){"(unbound)".to_owned()}else{row.accels.join(", ")}})).collect::<Vec<_>>()})
    }
}
fn split_accels(raw: &str) -> Vec<String> {
    raw.split([',', '\r', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}
