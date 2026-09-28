// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned nonmodal Options; callbacks queue actions and never mutate App directly.
use super::*;
use std::cell::Cell;
use windows_sys::Win32::{
    System::SystemServices::SS_NOPREFIX,
    UI::{
        Controls::{SetScrollInfo, EM_LIMITTEXT},
        Input::KeyboardAndMouse::EnableWindow,
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
    },
};
thread_local! {
    static COMPOSING: Cell<isize> = const { Cell::new(0) };
    static SYNCING: Cell<bool> = const { Cell::new(false) };
}
const SAVE_TIMER: usize = 7;
const INPUT_BASE: usize = 200;

#[derive(Clone, Copy)]
pub(crate) enum UiAction {
    Tab(usize),
    Changed(usize),
    Save,
    Scroll(i32),
    ScrollTo(i32),
    Reveal(usize),
    Reset,
    Reload,
    Close,
    Layout,
}
fn emit(action: UiAction) {
    post(Event::OptionsUi(action));
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
        WM_CLOSE => {
            emit(UiAction::Close);
            0
        }
        WM_SIZE => {
            if GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_CHILD == 0 {
                emit(UiAction::Layout);
            }
            0
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(lparam as *mut MINMAXINFO);
            let scale = GetDpiForWindow(window).max(96) as f64 / 96.0;
            info.ptMinTrackSize.x = (620.0 * scale) as i32;
            info.ptMinTrackSize.y = (400.0 * scale) as i32;
            0
        }
        WM_DPICHANGED => {
            let r = &*(lparam as *const RECT);
            SetWindowPos(
                window,
                std::ptr::null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            emit(UiAction::Layout);
            0
        }
        WM_TIMER if wparam == SAVE_TIMER => {
            KillTimer(window, SAVE_TIMER);
            emit(UiAction::Save);
            0
        }
        WM_MOUSEWHEEL => {
            emit(UiAction::Scroll(-((wparam >> 16) as i16 as i32) / 120 * 96));
            0
        }
        WM_VSCROLL => {
            let mut info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_ALL,
                ..Default::default()
            };
            GetScrollInfo(window, SB_VERT, &mut info);
            match (wparam & 0xffff) as i32 {
                SB_LINEUP => emit(UiAction::Scroll(-32)),
                SB_LINEDOWN => emit(UiAction::Scroll(32)),
                SB_PAGEUP => emit(UiAction::Scroll(-(info.nPage as i32))),
                SB_PAGEDOWN => emit(UiAction::Scroll(info.nPage as i32)),
                SB_TOP => emit(UiAction::ScrollTo(0)),
                SB_BOTTOM => emit(UiAction::ScrollTo(info.nMax)),
                SB_THUMBTRACK | SB_THUMBPOSITION => emit(UiAction::ScrollTo(info.nTrackPos)),
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            let id = wparam & 0xffff;
            let code = (wparam >> 16) as u32;
            if (INPUT_BASE..INPUT_BASE + 10).contains(&id) {
                if !SYNCING.with(Cell::get) && matches!(code, EN_CHANGE | CBN_SELCHANGE) {
                    emit(UiAction::Changed(id - INPUT_BASE));
                } else if matches!(code, EN_SETFOCUS | CBN_SETFOCUS) {
                    emit(UiAction::Reveal(id - INPUT_BASE));
                }
            } else if code == BN_CLICKED {
                match id {
                    1 => emit(UiAction::Tab(0)),
                    2 => emit(UiAction::Tab(1)),
                    3 => emit(UiAction::Reset),
                    4 => emit(UiAction::Close),
                    5 => emit(UiAction::Reload),
                    _ => {}
                }
            }
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}
unsafe extern "system" fn edit_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    if message == WM_IME_STARTCOMPOSITION {
        COMPOSING.with(|c| c.set(window as isize));
    }
    let result = DefSubclassProc(window, message, wparam, lparam);
    match message {
        WM_IME_ENDCOMPOSITION => {
            COMPOSING.with(|c| c.set(0));
            emit(UiAction::Changed(id));
        }
        WM_NCDESTROY => {
            if COMPOSING.with(Cell::get) == window as isize {
                COMPOSING.with(|c| c.set(0));
            }
            RemoveWindowSubclass(window, Some(edit_proc), id);
        }
        _ => {}
    }
    result
}

struct Row {
    key: Option<SettingKey>,
    page: usize,
    label: HWND,
    input: HWND,
    error: Option<String>,
    due: Option<Instant>,
    choices: Vec<(&'static str, &'static str)>,
    baseline: String,
}
pub(crate) struct Panel {
    pub(super) edit_id: Uuid,
    window: HWND,
    tabs: [HWND; 2],
    heading: HWND,
    viewport: HWND,
    groups: Vec<(usize, HWND)>,
    scroll: Cell<i32>,
    status: HWND,
    reset: HWND,
    close: HWND,
    reload: HWND,
    rows: Vec<Row>,
    page: usize,
    open: bool,
    pending: Option<(usize, String)>,
    shell: crate::shell::Shell,
}
impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
impl Panel {
    pub(super) fn new(parent: HWND) -> anyhow::Result<Self> {
        unsafe {
            let class = wide("flowmux.windows.options");
            let instance = GetModuleHandleW(std::ptr::null());
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: std::ptr::null_mut(),
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register Options window"
            );
            let scale = GetDpiForWindow(parent).max(96) as f64 / 96.0;
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Options").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                (760.0 * scale) as i32,
                (720.0 * scale) as i32,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create Options window");
            let mut p = Self {
                edit_id: Uuid::nil(),
                window,
                tabs: [std::ptr::null_mut(); 2],
                heading: std::ptr::null_mut(),
                viewport: std::ptr::null_mut(),
                groups: vec![],
                scroll: Cell::new(0),
                status: std::ptr::null_mut(),
                reset: std::ptr::null_mut(),
                close: std::ptr::null_mut(),
                reload: std::ptr::null_mut(),
                rows: Vec::new(),
                page: 0,
                open: false,
                pending: None,
                shell: Default::default(),
            };
            p.tabs = [
                p.child(
                    "BUTTON",
                    "General",
                    1,
                    WS_TABSTOP | BS_PUSHLIKE as u32 | BS_AUTORADIOBUTTON as u32 | WS_GROUP,
                )?,
                p.child(
                    "BUTTON",
                    "Theme",
                    2,
                    WS_TABSTOP | BS_PUSHLIKE as u32 | BS_AUTORADIOBUTTON as u32,
                )?,
            ];
            p.heading = p.child("STATIC", "", 20, SS_NOPREFIX)?;
            p.status = p.child("STATIC", "", 21, SS_NOPREFIX)?;
            p.reset = p.child("BUTTON", "Reset to defaults", 3, WS_TABSTOP)?;
            p.close = p.child("BUTTON", "Close", 4, WS_TABSTOP)?;
            p.reload = p.child("BUTTON", "Reload values", 5, WS_TABSTOP)?;
            p.viewport = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Options settings").as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_CLIPCHILDREN,
                0,
                0,
                1,
                1,
                p.window,
                22usize as HMENU,
                instance,
                std::ptr::null(),
            );
            checked((!p.viewport.is_null()) as i32)?;
            for (page, label) in [(0, "Terminal"), (0, "Minimap"), (0, "Shell"), (1, "Colors")] {
                let hwnd = p.child_in(
                    p.viewport,
                    "STATIC",
                    label,
                    400 + p.groups.len(),
                    SS_NOPREFIX,
                )?;
                chrome::register_control(hwnd, chrome::ControlRole::Caption);
                p.groups.push((page, hwnd));
            }
            p.row(
                Some(SettingKey::FontFamily),
                0,
                "Font family / fallbacks",
                vec![],
            )?;
            p.row(Some(SettingKey::FontSize), 0, "Font size (6–72 px)", vec![])?;
            p.row(
                Some(SettingKey::Scrollback),
                0,
                "Scrollback (0–100000 lines)",
                vec![],
            )?;
            p.row(
                Some(SettingKey::CursorBlink),
                0,
                "Cursor blink",
                vec![("On", "true"), ("Off", "false")],
            )?;
            p.row(
                Some(SettingKey::CursorStyle),
                0,
                "Cursor shape",
                vec![
                    ("Block", "block"),
                    ("Underline", "underline"),
                    ("Bar", "bar"),
                ],
            )?;
            p.row(
                Some(SettingKey::MinimapEnabled),
                0,
                "Minimap",
                vec![("On", "true"), ("Off", "false")],
            )?;
            p.row(
                Some(SettingKey::MinimapWidth),
                0,
                "Minimap width (12–96 px)",
                vec![],
            )?;
            p.row(
                Some(SettingKey::MinimapOpacity),
                0,
                "Minimap opacity (0–100%)",
                vec![],
            )?;
            p.row(None, 0, "Default shell (future terminals)", vec![])?;
            p.row(
                Some(SettingKey::Theme),
                1,
                "Color theme",
                vec![("Dark", "dark"), ("Light", "light")],
            )?;
            p.layout();
            Ok(p)
        }
    }
    fn child(&self, class: &str, text: &str, id: usize, style: u32) -> anyhow::Result<HWND> {
        self.child_in(self.window, class, text, id, style)
    }
    fn child_in(
        &self,
        parent: HWND,
        class: &str,
        text: &str,
        id: usize,
        style: u32,
    ) -> anyhow::Result<HWND> {
        let style = if class == "BUTTON" {
            (style & !0xf) | BS_OWNERDRAW as u32
        } else {
            style
        };
        unsafe {
            let hwnd = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                1,
                1,
                parent,
                id as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            checked((!hwnd.is_null()) as i32)?;
            if class == "BUTTON" {
                chrome::register_button(hwnd, chrome::Role::Button);
            } else {
                chrome::register_control(
                    hwnd,
                    if class == "STATIC" {
                        chrome::ControlRole::Static
                    } else {
                        chrome::ControlRole::Edit
                    },
                );
            }
            Ok(hwnd)
        }
    }
    fn row(
        &mut self,
        key: Option<SettingKey>,
        page: usize,
        title: &str,
        choices: Vec<(&'static str, &'static str)>,
    ) -> anyhow::Result<()> {
        let index = self.rows.len();
        let label = self.child_in(self.viewport, "STATIC", title, 300 + index, SS_NOPREFIX)?;
        let input = if choices.is_empty() {
            self.child_in(
                self.viewport,
                "EDIT",
                "",
                INPUT_BASE + index,
                WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
            )?
        } else {
            self.child_in(
                self.viewport,
                "COMBOBOX",
                "",
                INPUT_BASE + index,
                WS_TABSTOP | CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            )?
        };
        unsafe {
            if choices.is_empty() {
                SendMessageW(input, EM_LIMITTEXT, 256, 0);
                checked(SetWindowSubclass(input, Some(edit_proc), index, 0))?;
            } else {
                for (label, _) in &choices {
                    SendMessageW(input, CB_ADDSTRING, 0, wide(label).as_ptr() as LPARAM);
                }
            }
        }
        self.rows.push(Row {
            key,
            page,
            label,
            input,
            error: None,
            due: None,
            choices,
            baseline: String::new(),
        });
        Ok(())
    }
    fn text(hwnd: HWND) -> String {
        unsafe {
            let mut data = vec![0u16; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
            let n = GetWindowTextW(hwnd, data.as_mut_ptr(), data.len() as i32);
            String::from_utf16_lossy(&data[..n.max(0) as usize])
        }
    }
    fn value(row: &Row) -> String {
        if row.choices.is_empty() {
            Self::text(row.input)
        } else {
            let index = unsafe { SendMessageW(row.input, CB_GETCURSEL, 0, 0) };
            row.choices
                .get(index as usize)
                .map(|(_, value)| (*value).to_owned())
                .unwrap_or_default()
        }
    }
    fn set(row: &Row, value: &str) {
        SYNCING.with(|sync| sync.set(true));
        unsafe {
            if row.choices.is_empty() {
                SetWindowTextW(row.input, wide(value).as_ptr());
            } else {
                let index = row
                    .choices
                    .iter()
                    .position(|(_, v)| *v == value)
                    .unwrap_or(0);
                SendMessageW(row.input, CB_SETCURSEL, index, 0);
            }
        }
        SYNCING.with(|sync| sync.set(false));
    }
    pub(super) fn show(
        &mut self,
        document: &crate::settings::Document,
        error: Option<&str>,
        background: bool,
    ) {
        chrome::window_theme(self.window, document.terminal.theme);
        if !self.open {
            // A close may have flushed committed drafts to the worker. Keep its
            // generation until completion; never reload over a newer edit.
            if self.pending.is_none()
                && COMPOSING.with(Cell::get) == 0
                && self.rows.iter().all(|row| {
                    row.due.is_none() && row.error.is_none() && Self::value(row) == row.baseline
                })
            {
                self.edit_id = Uuid::new_v4();
                self.reload(document, error);
            }
            self.open = true;
        }
        if !background {
            unsafe {
                ShowWindow(self.window, SW_SHOW);
                SetFocus(self.tabs[self.page]);
            }
        }
    }
    pub(super) fn reload(&mut self, document: &crate::settings::Document, error: Option<&str>) {
        self.shell = document.default_shell.clone();
        for row in &mut self.rows {
            let value = row
                .key
                .map(|key| document.terminal.value(key))
                .unwrap_or_else(|| document.default_shell.program.clone());
            Self::set(row, &value);
            row.baseline = value;
            row.error = None;
            row.due = None;
        }
        self.status(
            error.unwrap_or("Changes save automatically and apply to all flowmux windows."),
        );
        self.enable();
        self.layout();
    }
    pub(super) fn status(&self, text: &str) {
        unsafe {
            SetWindowTextW(self.status, wide(text).as_ptr());
        }
    }
    pub(super) fn select(&mut self, page: usize) {
        if page < 2 {
            self.page = page;
            self.scroll.set(0);
            self.layout();
        }
    }
    pub(super) fn changed(&mut self, index: usize) {
        // IME may finish while ShowWindow(SW_HIDE) closes the Options window.
        // Its committed text still belongs to this edit generation and must be
        // queued, rather than discarded when the dialog is next opened.
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        row.error = None;
        row.due = Some(
            Instant::now()
                + if row.choices.is_empty() {
                    Duration::from_millis(250)
                } else {
                    Duration::ZERO
                },
        );
        self.schedule();
    }
    fn schedule(&self) {
        if self.pending.is_none() && self.rows.iter().any(|row| row.due.is_some()) {
            unsafe {
                SetTimer(self.window, SAVE_TIMER, 50, None);
            }
        }
    }
    pub(super) fn next_due(&mut self) -> Option<usize> {
        if self.pending.is_some() || COMPOSING.with(Cell::get) != 0 {
            return None;
        }
        for (index, row) in self.rows.iter_mut().enumerate() {
            if row.due.is_some_and(|at| at <= Instant::now()) {
                row.due = None;
                if Self::value(row) != row.baseline {
                    return Some(index);
                }
            }
        }
        self.schedule();
        None
    }
    pub(super) fn operation(
        &self,
        index: usize,
        document: &crate::settings::Document,
    ) -> anyhow::Result<SettingsOp> {
        anyhow::ensure!(self.pending.is_none(), "a setting is still being saved");
        anyhow::ensure!(
            COMPOSING.with(Cell::get) == 0,
            "finish text composition before changing a setting"
        );
        let row = self.rows.get(index).context("Options field not found")?;
        let value = Self::value(row);
        if let Some(key) = row.key {
            // Validate drafts locally so invalid typing never becomes a disk/config error.
            document
                .terminal
                .changed(key, &value, Some(&row.baseline))?;
            Ok(SettingsOp::Set {
                key,
                value,
                expected: Some(row.baseline.clone()),
            })
        } else {
            anyhow::ensure!(
                self.shell == document.default_shell,
                "default shell changed elsewhere; reload values"
            );
            let shell = crate::shell::Shell {
                args: if value == self.shell.program {
                    self.shell.args.clone()
                } else {
                    Vec::new()
                },
                program: value,
            };
            // Availability is resolved by the existing worker, not with disk or
            // network I/O on the UI thread. Syntax is safe to validate here.
            shell.validate()?;
            Ok(SettingsOp::Shell {
                program: shell.program,
                args: shell.args,
                expected: Some(self.shell.clone()),
            })
        }
    }
    pub(super) fn begin(&mut self, index: usize) {
        let value = self.rows.get(index).map(Self::value).unwrap_or_default();
        self.pending = Some((index, value));
        if index == usize::MAX {
            for row in &mut self.rows {
                row.due = None;
                row.error = None;
            }
        }
        self.status("Saving…");
        self.enable();
    }
    pub(super) fn failed(&mut self, index: usize, error: &str) {
        if let Some(row) = self.rows.get_mut(index) {
            row.error = Some(error.into());
        }
        self.status(error);
        self.schedule();
    }
    pub(super) fn update(
        &mut self,
        document: &crate::settings::Document,
        error: Option<&str>,
        completed: bool,
    ) {
        chrome::window_theme(self.window, document.terminal.theme);
        unsafe {
            InvalidateRect(self.window, std::ptr::null(), 1);
        }
        let applied = if completed { self.pending.take() } else { None };
        let mut shell_baseline_updated = false;
        for (index, row) in self.rows.iter_mut().enumerate() {
            let value = row
                .key
                .map(|key| document.terminal.value(key))
                .unwrap_or_else(|| document.default_shell.program.clone());
            let current = Self::value(row);
            let composing = COMPOSING.with(Cell::get) == row.input as isize;
            let own = applied
                .as_ref()
                .filter(|(i, _)| *i == index || *i == usize::MAX);
            if own.is_some() && error.is_none() {
                // Advance CAS baseline for our saved value, but retain subsequent typing.
                let unchanged =
                    own.is_some_and(|(i, submitted)| *i == usize::MAX || current == *submitted);
                if unchanged && !composing {
                    Self::set(row, &value);
                    row.due = None;
                }
                row.baseline = value;
                shell_baseline_updated |= index == 8;
                row.error = None;
            } else if !composing && current == row.baseline && row.due.is_none() {
                Self::set(row, &value);
                row.baseline = value;
                shell_baseline_updated |= index == 8;
            }
            if own.is_some() && error.is_some() {
                row.error = error.map(str::to_owned);
            }
        }
        if shell_baseline_updated {
            self.shell = document.default_shell.clone();
        }
        let draft_error = self.rows.iter().find_map(|row| row.error.as_deref());
        if completed || self.pending.is_none() {
            self.status(
                draft_error
                    .or(error)
                    .unwrap_or("Changes save automatically and apply to all flowmux windows."),
            );
        }
        self.enable();
        self.schedule();
    }
    fn enable(&self) {
        unsafe {
            // Keep EDIT enabled while I/O is pending: disabling it can end IME
            // composition and steal focus. Reset alone freezes inputs briefly.
            let resetting = self
                .pending
                .as_ref()
                .is_some_and(|(index, _)| *index == usize::MAX);
            for row in &self.rows {
                EnableWindow(row.input, i32::from(!resetting));
            }
            EnableWindow(self.reset, i32::from(self.pending.is_none()));
            EnableWindow(self.reload, i32::from(self.pending.is_none()));
        }
    }
    pub(super) fn reset_ready(&self) -> bool {
        self.open && self.pending.is_none() && COMPOSING.with(Cell::get) == 0
    }
    pub(super) fn hide(&mut self) {
        self.open = false;
        for row in &mut self.rows {
            if row.due.is_some() {
                row.due = Some(Instant::now());
            }
        }
        unsafe {
            ShowWindow(self.window, SW_HIDE);
        }
        self.schedule();
    }
    pub(super) fn scroll_by(&self, delta: i32) {
        self.scroll.set(self.scroll.get().saturating_add(delta));
        self.layout();
    }
    pub(super) fn scroll_to(&self, position: i32) {
        self.scroll.set(position);
        self.layout();
    }
    pub(super) fn reveal(&self, index: usize) {
        if let Some(row) = self.rows.get(index).filter(|row| row.page == self.page) {
            unsafe {
                let mut rect = RECT::default();
                GetWindowRect(row.input, &mut rect);
                let mut viewport = RECT::default();
                GetWindowRect(self.viewport, &mut viewport);
                if rect.top < viewport.top {
                    self.scroll_by(rect.top - viewport.top - 8);
                } else if rect.top + 30 > viewport.bottom {
                    self.scroll_by(rect.top + 30 - viewport.bottom + 8);
                }
            }
        }
    }
    pub(super) fn layout(&self) {
        unsafe {
            let mut client = RECT::default();
            GetClientRect(self.window, &mut client);
            let scale = GetDpiForWindow(self.window).max(96) as f64 / 96.0;
            let px = |n: i32| (n as f64 * scale).round() as i32;
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
            };
            for (i, tab) in self.tabs.iter().enumerate() {
                chrome::set_role(
                    *tab,
                    chrome::Role::Workspace {
                        selected: i == self.page,
                        color: None,
                    },
                );
                InvalidateRect(*tab, std::ptr::null(), 1);
                place(*tab, px(20 + i as i32 * 140), px(16), px(132), px(32));
                SendMessageW(*tab, BM_SETCHECK, usize::from(i == self.page), 0);
            }
            SetWindowTextW(
                self.heading,
                wide(if self.page == 0 {
                    "Terminal appearance and behavior"
                } else {
                    "Terminal and native window colors"
                })
                .as_ptr(),
            );
            place(self.heading, px(20), px(60), client.right - px(40), px(26));
            let viewport_height = (client.bottom - px(190)).max(px(80));
            place(
                self.viewport,
                px(20),
                px(92),
                client.right - px(40),
                viewport_height,
            );
            let content_height = if self.page == 0 { px(562) } else { px(94) };
            let offset = self
                .scroll
                .get()
                .clamp(0, (content_height - viewport_height).max(0));
            self.scroll.set(offset);
            let info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                nMin: 0,
                nMax: content_height - 1,
                nPage: viewport_height as u32,
                nPos: offset,
                ..Default::default()
            };
            SetScrollInfo(self.viewport, SB_VERT, &info, 1);
            let mut view = RECT::default();
            GetClientRect(self.viewport, &mut view);
            for (index, (page, group)) in self.groups.iter().enumerate() {
                ShowWindow(
                    *group,
                    if *page == self.page {
                        SW_SHOWNA
                    } else {
                        SW_HIDE
                    },
                );
                let top = match index {
                    0 | 3 => 8,
                    1 => 286,
                    _ => 468,
                };
                place(*group, px(8), px(top) - offset, view.right - px(16), px(26));
            }
            for (index, row) in self.rows.iter().enumerate() {
                let show = row.page == self.page;
                for hwnd in [row.label, row.input] {
                    ShowWindow(hwnd, if show { SW_SHOWNA } else { SW_HIDE });
                }
                if show {
                    let top = match index {
                        0..=4 => 44 + index as i32 * 46,
                        5..=7 => 322 + (index as i32 - 5) * 46,
                        8 => 504,
                        _ => 44,
                    };
                    place(row.label, px(8), px(top) - offset, px(230), px(30));
                    place(
                        row.input,
                        px(246),
                        px(top) - offset,
                        view.right - px(254),
                        if row.choices.is_empty() {
                            px(30)
                        } else {
                            px(160)
                        },
                    );
                }
            }
            place(
                self.status,
                px(20),
                client.bottom - px(92),
                client.right - px(40),
                px(42),
            );
            place(self.reset, px(20), client.bottom - px(40), px(155), px(28));
            place(
                self.reload,
                px(184),
                client.bottom - px(40),
                px(125),
                px(28),
            );
            place(
                self.close,
                client.right - px(110),
                client.bottom - px(40),
                px(90),
                px(28),
            );
        }
    }
    pub(super) fn diagnostics(&self) -> Value {
        json!({"window":self.window as usize,"owner":unsafe{GetWindow(self.window,GW_OWNER)} as usize,"open":self.open,"native_visible":unsafe{IsWindowVisible(self.window)}!=0,"modal":false,"page":if self.page==0{"general"}else{"theme"},"pending":self.pending.is_some(),"auto_apply":true,"queued":self.rows.iter().filter(|row|row.due.is_some()).count(),"composing":COMPOSING.with(Cell::get)!=0,"viewport":self.viewport as usize,"scroll_offset":self.scroll.get(),"error_or_status":Self::text(self.status),"tabs":[{"name":"General","handle":self.tabs[0] as usize},{"name":"Theme","handle":self.tabs[1] as usize}],"reset":self.reset as usize,"reload":self.reload as usize,"close":self.close as usize,"controls":self.rows.iter().map(|row|json!({"key":row.key.map(|key|serde_json::to_value(key).unwrap()).unwrap_or(json!("default_shell")),"label":Self::text(row.label),"input":row.input as usize,"parent":self.viewport as usize,"draft_error":row.error,"page":if row.page==0{"general"}else{"theme"},"value":Self::value(row),"baseline":row.baseline})).collect::<Vec<_>>()})
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        if COMPOSING.with(Cell::get) != 0 || message.wParam == 229 {
            return false;
        }
        unsafe {
            if !self.open
                || IsWindowVisible(self.window) == 0
                || (message.hwnd != self.window && IsChild(self.window, message.hwnd) == 0)
            {
                return false;
            }
            let dropdown = self.rows.iter().any(|row| {
                !row.choices.is_empty() && SendMessageW(row.input, CB_GETDROPPEDSTATE, 0, 0) != 0
            });
            if message.message == WM_KEYDOWN && message.wParam == 27 && !dropdown {
                emit(UiAction::Close);
                return true;
            }
            if self.rows.iter().any(|row| {
                row.choices.is_empty()
                    && message.hwnd == row.input
                    && matches!(message.message, WM_KEYDOWN | WM_KEYUP | WM_CHAR)
                    && message.wParam == 13
            }) {
                return false;
            }
            IsDialogMessageW(self.window, message) != 0
        }
    }
}
