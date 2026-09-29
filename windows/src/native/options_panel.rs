// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned nonmodal Options; callbacks queue actions and never mutate App directly.
use super::*;
use std::cell::Cell;
use windows_sys::Win32::{
    System::SystemServices::SS_NOPREFIX,
    UI::{
        Controls::{SetScrollInfo, SetWindowTheme, EM_LIMITTEXT},
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
const LABEL_BASE: usize = 300;
#[path = "keybindings_panel.rs"]
mod bindings;
#[path = "font_picker.rs"]
mod fonts;
#[path = "theme_panel.rs"]
mod themes;

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
    About,
    AboutClosed(Uuid),
    FocusColor,
    Layout,
    Bindings(bindings::Signal),
    FontPicker(fonts::Signal),
    Theme(themes::Signal),
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
        WM_TIMER if wparam == fonts::TIMER => {
            emit(UiAction::FontPicker(fonts::Signal::Poll));
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
            if let Some(signal) = themes::command(id, code) {
                emit(UiAction::Theme(signal));
            } else if let Some(signal) = bindings::command(id, code) {
                emit(UiAction::Bindings(signal));
            } else if (INPUT_BASE..LABEL_BASE).contains(&id) {
                if !SYNCING.with(Cell::get)
                    && matches!(code, EN_CHANGE | CBN_SELCHANGE | BN_CLICKED)
                {
                    emit(UiAction::Changed(id - INPUT_BASE));
                } else if matches!(code, EN_SETFOCUS | CBN_SETFOCUS | BN_SETFOCUS) {
                    emit(UiAction::Reveal(id - INPUT_BASE));
                }
            } else if code == BN_CLICKED {
                match id {
                    1 => emit(UiAction::Tab(0)),
                    2 => emit(UiAction::Tab(1)),
                    6 => emit(UiAction::Tab(2)),
                    7 => emit(UiAction::FontPicker(fonts::Signal::Open)),
                    3 => emit(UiAction::Reset),
                    4 => emit(UiAction::Close),
                    5 => emit(UiAction::Reload),
                    8 => emit(UiAction::About),
                    9 => emit(UiAction::FocusColor),
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
            if let Some(signal) = bindings::command(id, EN_CHANGE) {
                emit(UiAction::Bindings(signal));
            } else {
                emit(UiAction::Changed(id));
            }
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
impl Row {
    fn is_toggle(&self) -> bool {
        matches!(self.choices.as_slice(), [(_, "true"), (_, "false")])
    }
}
pub(crate) struct Panel {
    pub(super) edit_id: Uuid,
    window: HWND,
    tabs: [HWND; 3],
    bindings: Option<bindings::Bindings>,
    font_picker: Option<fonts::Picker>,
    theme_panel: Option<themes::ThemePanel>,
    background: bool,
    theme: crate::settings::Theme,
    heading: HWND,
    viewport: HWND,
    theme_heading: HWND,
    browser_engine: [HWND; 2],
    scroll: Cell<i32>,
    status: HWND,
    status_is_help: Cell<bool>,
    reset: HWND,
    close: HWND,
    reload: HWND,
    about_button: HWND,
    about: Option<(Uuid, editor::ClosePanel)>,
    focus_color_button: HWND,
    focus_custom_colors: [COLORREF; 16],
    rows: Vec<Row>,
    page: usize,
    open: bool,
    pending: Option<(usize, String)>,
    shell: crate::shell::Shell,
}
impl Drop for Panel {
    fn drop(&mut self) {
        self.about.take();
        self.font_picker.take();
        if let Some(bindings) = self.bindings.as_mut() {
            bindings.dismiss();
        }
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
                tabs: [std::ptr::null_mut(); 3],
                bindings: None,
                font_picker: None,
                theme_panel: None,
                background: true,
                theme: crate::settings::Theme::Dark,
                heading: std::ptr::null_mut(),
                viewport: std::ptr::null_mut(),
                theme_heading: std::ptr::null_mut(),
                browser_engine: [std::ptr::null_mut(); 2],
                scroll: Cell::new(0),
                status: std::ptr::null_mut(),
                status_is_help: Cell::new(false),
                reset: std::ptr::null_mut(),
                close: std::ptr::null_mut(),
                reload: std::ptr::null_mut(),
                about_button: std::ptr::null_mut(),
                about: None,
                focus_color_button: std::ptr::null_mut(),
                focus_custom_colors: [0; 16],
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
                p.child("BUTTON", "Keybindings", 6, WS_TABSTOP)?,
            ];
            p.heading = p.child("STATIC", "", 20, SS_NOPREFIX)?;
            p.status = p.child("STATIC", "", 21, SS_NOPREFIX)?;
            p.reset = p.child("BUTTON", "Reset to defaults", 3, WS_TABSTOP)?;
            p.close = p.child("BUTTON", "Close", 4, WS_TABSTOP)?;
            p.reload = p.child("BUTTON", "Reload values", 5, WS_TABSTOP)?;
            p.about_button = p.child("BUTTON", "About", 8, WS_TABSTOP)?;
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
            p.theme_heading = p.child_in(p.viewport, "STATIC", "Colors", 400, SS_NOPREFIX)?;
            chrome::register_control(p.theme_heading, chrome::ControlRole::Caption);
            p.row(Some(SettingKey::ZoomPercent), 0, "Global zoom (%)", vec![])?;
            p.row(Some(SettingKey::FontFamily), 0, "Terminal font", vec![])?;
            p.row(Some(SettingKey::FontSize), 0, "Font size (px)", vec![])?;
            p.browser_engine = [
                p.child_in(p.viewport, "STATIC", "Browser web view", 401, SS_NOPREFIX)?,
                p.child_in(p.viewport, "STATIC", "WebView2", 402, SS_NOPREFIX)?,
            ];
            p.row(
                Some(SettingKey::FocusBorderColor),
                0,
                "Focus border color",
                vec![],
            )?;
            p.row(
                Some(SettingKey::FocusBorderOpacity),
                0,
                "Focus border opacity (%)",
                vec![],
            )?;
            p.row(
                Some(SettingKey::PersistBrowserSession),
                0,
                "Keep browser session data",
                vec![("On", "true"), ("Off", "false")],
            )?;
            p.row(
                Some(SettingKey::RestoreTerminalScrollback),
                0,
                "Restore terminal scrollback",
                vec![("On", "true"), ("Off", "false")],
            )?;
            p.row(
                Some(SettingKey::Scrollback),
                0,
                "Terminal scrollback lines",
                vec![],
            )?;
            p.row(
                Some(SettingKey::MinimapEnabled),
                0,
                "Terminal minimap",
                vec![("On", "true"), ("Off", "false")],
            )?;
            p.row(
                Some(SettingKey::MinimapWidth),
                0,
                "Terminal minimap width (px)",
                vec![],
            )?;
            p.row(
                Some(SettingKey::MinimapOpacity),
                0,
                "Terminal minimap opacity (%)",
                vec![],
            )?;
            p.row(None, 0, "Default shell (future terminals)", vec![])?;
            p.row(
                Some(SettingKey::SystemNotificationsEnabled),
                0,
                "System notifications",
                vec![("On", "true"), ("Off", "false")],
            )?;
            p.row(
                Some(SettingKey::AgentBarMode),
                0,
                "Agents bar mode",
                vec![("On", "true"), ("Off", "false")],
            )?;
            p.row(
                Some(SettingKey::UsageBarEnabled),
                0,
                "AI Usage bar",
                vec![("On", "true"), ("Off", "false")],
            )?;
            p.row(
                Some(SettingKey::AgentNotificationTarget),
                0,
                "Agent notification target",
                vec![
                    ("Agent bar", "agent_bar"),
                    ("Workspace", "workspace"),
                    ("Both", "both"),
                ],
            )?;
            p.row(
                Some(SettingKey::EditorMinimapEnabled),
                0,
                "Editor minimap",
                vec![("On", "true"), ("Off", "false")],
            )?;
            p.row(
                Some(SettingKey::CursorBlink),
                0,
                "Cursor blink",
                vec![("On", "true"), ("Off", "false")],
            )?;
            p.row(
                Some(SettingKey::CursorBlinkIntervalMs),
                0,
                "Cursor blink interval (ms)",
                vec![],
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
                Some(SettingKey::Theme),
                1,
                "Legacy dark/light",
                vec![("Dark", "dark"), ("Light", "light")],
            )?;
            p.row(Some(SettingKey::ThemePreset), 1, "Theme preset", vec![])?;
            for (key, label) in [
                (SettingKey::ThemeBackground, "Terminal background"),
                (SettingKey::ThemeForeground, "Terminal text"),
                (SettingKey::ThemeCursor, "Cursor"),
                (SettingKey::ThemeSelectionBackground, "Selection background"),
                (SettingKey::ThemeSelectionForeground, "Selection text"),
            ] {
                p.row(Some(key), 1, label, vec![])?;
            }
            p.row(Some(SettingKey::ThemeOverrides), 1, "Custom colors", vec![])?;
            p.theme_panel = Some(themes::ThemePanel::new(&p)?);
            p.bindings = Some(bindings::Bindings::new(&p)?);
            let font_entry = p.child_in(p.viewport, "BUTTON", "Choose…", 7, WS_TABSTOP)?;
            p.font_picker = Some(fonts::Picker::new(p.window, font_entry));
            p.focus_color_button = p.child_in(p.viewport, "BUTTON", "Choose…", 9, WS_TABSTOP)?;
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
        let style = if class == "BUTTON" && style & 0xf != BS_AUTOCHECKBOX as u32 {
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
            if class == "BUTTON" && style & 0xf == BS_OWNERDRAW as u32 {
                chrome::register_button(hwnd, chrome::Role::Button);
            } else {
                chrome::register_control(
                    hwnd,
                    if matches!(class, "STATIC" | "BUTTON") {
                        chrome::ControlRole::Static
                    } else if class == "LISTBOX" {
                        chrome::ControlRole::Listbox
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
        let label = self.child_in(
            self.viewport,
            "STATIC",
            title,
            LABEL_BASE + index,
            SS_NOPREFIX,
        )?;
        let toggle = matches!(choices.as_slice(), [(_, "true"), (_, "false")]);
        let input = if toggle {
            // Keep native check state, Space handling and the accessible setting name.
            self.child_in(
                self.viewport,
                "BUTTON",
                title,
                INPUT_BASE + index,
                WS_TABSTOP | (BS_AUTOCHECKBOX | BS_NOTIFY | BS_LEFTTEXT | BS_LEFT) as u32,
            )?
        } else if choices.is_empty() {
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
            if toggle {
                // Native themed checkbox text ignores our dark palette's WM_CTLCOLOR.
                SetWindowTheme(input, wide("").as_ptr(), wide("").as_ptr());
            } else if choices.is_empty() {
                SendMessageW(input, EM_LIMITTEXT, 256, 0);
                checked(SetWindowSubclass(input, Some(edit_proc), index, 0))?;
            } else if !toggle {
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
            baseline: if toggle {
                "false".into()
            } else {
                String::new()
            },
        });
        Ok(())
    }
    fn index(&self, key: SettingKey) -> usize {
        self.rows
            .iter()
            .position(|row| row.key == Some(key))
            .expect("Options setting exists")
    }
    fn text(hwnd: HWND) -> String {
        unsafe {
            let mut data = vec![0u16; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
            let n = GetWindowTextW(hwnd, data.as_mut_ptr(), data.len() as i32);
            String::from_utf16_lossy(&data[..n.max(0) as usize])
        }
    }
    fn value(row: &Row) -> String {
        if row.is_toggle() {
            (unsafe { SendMessageW(row.input, BM_GETCHECK, 0, 0) } == 1).to_string()
        } else if row.choices.is_empty() {
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
            if row.is_toggle() {
                SendMessageW(row.input, BM_SETCHECK, usize::from(value == "true"), 0);
            } else if row.choices.is_empty() {
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
        self.background = background;
        self.theme = if crate::theme::resolve(&document.terminal).dark {
            crate::settings::Theme::Dark
        } else {
            crate::settings::Theme::Light
        };
        chrome::window_theme(self.window, self.theme);
        if let Some(bindings) = self.bindings.as_mut() {
            bindings.environment(background, self.theme);
        }
        if !self.open {
            // A close may have flushed committed drafts to the worker. Keep its
            // generation until completion; never reload over a newer edit.
            if self.pending.is_none()
                && COMPOSING.with(Cell::get) == 0
                && !self
                    .bindings
                    .as_ref()
                    .is_some_and(bindings::Bindings::has_draft)
                && self.rows.iter().all(|row| {
                    row.due.is_none() && row.error.is_none() && Self::value(row) == row.baseline
                })
            {
                self.edit_id = Uuid::new_v4();
                self.reload(document, error);
            }
            self.open = true;
        }
        if self
            .font_picker
            .as_ref()
            .is_some_and(fonts::Picker::is_open)
        {
            self.font_picker.as_ref().unwrap().focus();
            return;
        }
        if self
            .bindings
            .as_ref()
            .is_some_and(bindings::Bindings::modal)
        {
            self.bindings.as_ref().unwrap().focus();
            return;
        }
        if let Some((_, about)) = &self.about {
            chrome::window_theme(about.window(), self.theme);
            if !background {
                unsafe {
                    SetForegroundWindow(about.window());
                }
            }
            return;
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
        if let Some(theme) = self.theme_panel.as_mut() {
            theme.reset_snapshot = None;
            theme.sync(&document.terminal);
        }
        if let Some(bindings) = self.bindings.as_mut() {
            bindings.reload(document);
        }
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
        if let Some(error) = error {
            self.status(error);
        } else {
            self.default_status();
        }
        self.enable();
        self.layout();
    }
    pub(super) fn status(&self, text: &str) {
        self.status_is_help.set(false);
        unsafe {
            SetWindowTextW(self.status, wide(text).as_ptr());
        }
    }
    fn default_status(&self) {
        self.status(if self.page == 2 {
            "Shortcut edits save after OK. Reset and Unbind in the edit dialog only change the draft."
        } else {
            "Changes save automatically and apply to all flowmux windows."
        });
        self.status_is_help.set(true);
    }
    pub(super) fn select(&mut self, page: usize) {
        if page < 3
            && self.about.is_none()
            && !self
                .font_picker
                .as_ref()
                .is_some_and(fonts::Picker::is_open)
            && !self
                .bindings
                .as_ref()
                .is_some_and(bindings::Bindings::modal)
        {
            self.page = page;
            if self.status_is_help.get() {
                self.default_status();
            }
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
        if self.pending.is_none()
            && (self.rows.iter().any(|row| row.due.is_some())
                || self
                    .bindings
                    .as_ref()
                    .is_some_and(bindings::Bindings::queued))
        {
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
        if self
            .bindings
            .as_ref()
            .is_some_and(bindings::Bindings::queued)
        {
            return Some(bindings::SAVE_INDEX);
        }
        self.schedule();
        None
    }
    pub(super) fn bindings_signal(&mut self, signal: bindings::Signal) {
        if let bindings::Signal::Reveal(index) = signal {
            let delta = self
                .bindings
                .as_ref()
                .map_or(0, |bindings| bindings.reveal_delta(index));
            if delta != 0 {
                self.scroll_by(delta);
            }
            return;
        }
        if let Some(bindings) = self.bindings.as_mut() {
            bindings.signal(signal, self.pending.is_none());
        }
        if let Some(error) = self.bindings.as_ref().and_then(bindings::Bindings::error) {
            self.status(error);
        }
        self.enable();
        self.schedule();
    }
    pub(super) fn font_picker_signal(
        &mut self,
        signal: fonts::Signal,
        document: &crate::settings::Document,
        error: Option<&str>,
    ) {
        let index = self.index(SettingKey::FontFamily);
        if matches!(signal, fonts::Signal::Open) {
            if !self.open
                || self.page != 0
                || self.pending.is_some()
                || self.rows[index].due.is_some()
                || COMPOSING.with(Cell::get) != 0
                || self
                    .bindings
                    .as_ref()
                    .is_some_and(bindings::Bindings::modal)
            {
                return;
            }
            let raw = Self::value(&self.rows[index]);
            if let Some(picker) = self.font_picker.as_mut() {
                if let Err(error) = picker.open(&raw, self.background, self.theme) {
                    self.status(&format!("{error:#}"));
                }
            }
            return;
        }
        let Some(picker) = self.font_picker.as_mut() else {
            return;
        };
        let was_open = picker.is_open();
        let snapshot = picker.source_raw().map(str::to_owned);
        let Some(result) = picker.signal(signal) else {
            if was_open && !picker.is_open() {
                // Cancelling an untouched draft reveals any external winner
                // that was deliberately held back while the modal was open.
                self.update(document, error, false);
            }
            return;
        };
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                picker.error(error);
                return;
            }
        };
        let current = Self::value(&self.rows[index]);
        if COMPOSING.with(Cell::get) != 0 || snapshot.as_deref() != Some(current.as_str()) {
            picker.error(
                "The original font field changed or is composing. Cancel and reopen the picker."
                    .into(),
            );
            return;
        }
        // The font row baseline is held while this modal is open. The existing
        // writer therefore rejects a stale choice after an external change.
        picker.close();
        if value == current {
            self.update(document, error, false);
            return;
        }
        Self::set(&self.rows[index], &value);
        if value == self.rows[index].baseline && value != document.terminal.font_family {
            // An explicit choice returning a dirty field to its old baseline
            // still conflicts with an external winner; next_due's normal no-op
            // optimization must not silently discard this case.
            self.rows[index].due = None;
            self.failed(
                index,
                "this setting changed elsewhere; reopen the editor before applying",
            );
            return;
        }
        self.changed(index);
    }
    pub(super) fn theme_signal(&mut self, signal: themes::Signal) {
        if let themes::Signal::Reveal(index) = signal {
            if let Some(theme) = self.theme_panel.as_ref() {
                self.scroll_by(theme.reveal_delta(self.viewport, index));
            }
            return;
        }
        if self.page != 1 || !self.reset_ready() {
            return;
        }
        match signal {
            themes::Signal::Preset(index) => {
                let value = self
                    .theme_panel
                    .as_ref()
                    .and_then(|theme| theme.preset(index))
                    .map(str::to_owned);
                if let Some(value) = value {
                    let row = self.index(SettingKey::ThemePreset);
                    Self::set(&self.rows[row], &value);
                    self.changed(row);
                    self.rows[row].due = Some(Instant::now());
                }
            }
            themes::Signal::Legacy => {
                let row = self.index(SettingKey::ThemePreset);
                Self::set(&self.rows[row], "");
                self.changed(row);
                self.rows[row].due = Some(Instant::now());
            }
            themes::Signal::Reset => {
                let snapshot = themes::COLORS.map(|key| Self::value(&self.rows[self.index(key)]));
                for key in themes::COLORS {
                    let index = self.index(key);
                    self.rows[index].due = None;
                }
                let overrides = self.index(SettingKey::ThemeOverrides);
                if self.rows[overrides].baseline == "{}" {
                    for key in themes::COLORS {
                        let index = self.index(key);
                        let row = &mut self.rows[index];
                        Self::set(row, "");
                        row.baseline.clear();
                        row.error = None;
                    }
                    self.default_status();
                    return;
                }
                if let Some(theme) = self.theme_panel.as_mut() {
                    theme.reset_snapshot = Some(snapshot);
                }
                Self::set(&self.rows[overrides], "{}");
                self.changed(overrides);
                self.rows[overrides].due = Some(Instant::now());
            }
            themes::Signal::Pick(index) if index < 5 => {
                let row_index = self.index(themes::COLORS[index]);
                let raw = Self::value(&self.rows[row_index]);
                let baseline = self.rows[row_index].baseline.clone();
                let result =
                    self.theme_panel
                        .as_mut()
                        .unwrap()
                        .choose(self.window, index, self.background);
                match result {
                    Ok(Some(value)) => {
                        if COMPOSING.with(Cell::get) != 0
                            || raw != Self::value(&self.rows[row_index])
                            || baseline != self.rows[row_index].baseline
                        {
                            self.status(
                                "The color field changed while the chooser was open; choose again.",
                            );
                            return;
                        }
                        Self::set(&self.rows[row_index], &value);
                        self.changed(row_index);
                        self.rows[row_index].due = Some(Instant::now());
                    }
                    Ok(None) => {}
                    Err(error) => self.status(&format!("{error:#}")),
                }
            }
            _ => {}
        }
    }
    pub(super) fn pick_focus_color(&mut self) {
        if self.page != 0 || !self.reset_ready() {
            return;
        }
        if self.background {
            self.status("The native color dialog is disabled during hidden verification; use the hex field.");
            return;
        }
        let index = self.index(SettingKey::FocusBorderColor);
        let raw = Self::value(&self.rows[index]);
        let baseline = self.rows[index].baseline.clone();
        let initial = if crate::theme::valid_color(&raw) {
            &raw
        } else {
            &baseline
        };
        match chrome::choose_color(
            self.window,
            chrome::color_ref(initial),
            &mut self.focus_custom_colors,
            self.background,
        ) {
            Ok(Some(color)) => {
                if COMPOSING.with(Cell::get) != 0
                    || raw != Self::value(&self.rows[index])
                    || baseline != self.rows[index].baseline
                {
                    self.status(
                        "The color field changed while the chooser was open; choose again.",
                    );
                    return;
                }
                Self::set(
                    &self.rows[index],
                    &format!(
                        "#{:02x}{:02x}{:02x}",
                        color & 255,
                        (color >> 8) & 255,
                        (color >> 16) & 255
                    ),
                );
                self.changed(index);
                self.rows[index].due = Some(Instant::now());
            }
            Ok(None) => {}
            Err(error) => self.status(&format!("{error:#}")),
        }
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
        if index == bindings::SAVE_INDEX {
            return self
                .bindings
                .as_ref()
                .context("Keybindings page unavailable")?
                .operation(document);
        }
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
        if index == bindings::SAVE_INDEX {
            if let Some(bindings) = self.bindings.as_mut() {
                bindings.begin();
            }
        }
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
        if self
            .rows
            .get(index)
            .is_some_and(|row| row.key == Some(SettingKey::ThemeOverrides))
        {
            if let Some(theme) = self.theme_panel.as_mut() {
                theme.reset_snapshot = None;
            }
        }
        if index == bindings::SAVE_INDEX {
            if let Some(bindings) = self.bindings.as_mut() {
                bindings.failed(error);
            }
        }
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
        self.theme = if crate::theme::resolve(&document.terminal).dark {
            crate::settings::Theme::Dark
        } else {
            crate::settings::Theme::Light
        };
        if let Some(picker) = self.font_picker.as_ref() {
            picker.theme(self.theme);
        }
        if let Some(bindings) = self.bindings.as_mut() {
            bindings.environment(self.background, self.theme);
        }
        chrome::window_theme(self.window, self.theme);
        if let Some((_, about)) = &self.about {
            chrome::window_theme(about.window(), self.theme);
            unsafe {
                InvalidateRect(about.window(), std::ptr::null(), 1);
            }
        }
        unsafe {
            InvalidateRect(self.window, std::ptr::null(), 1);
        }
        let applied = if completed { self.pending.take() } else { None };
        let reset_colors = applied.as_ref().is_some_and(|(index, _)| {
            self.rows
                .get(*index)
                .is_some_and(|row| row.key == Some(SettingKey::ThemeOverrides))
        });
        let reset_snapshot = self.theme_panel.as_mut().and_then(|theme| {
            theme.sync(&document.terminal);
            if reset_colors {
                theme.reset_snapshot.take()
            } else {
                None
            }
        });
        if let Some(bindings) = self.bindings.as_mut() {
            bindings.update(
                document,
                error,
                applied
                    .as_ref()
                    .is_some_and(|(index, _)| *index == bindings::SAVE_INDEX),
            );
        }
        let mut shell_baseline_updated = false;
        let font_picker_open = self
            .font_picker
            .as_ref()
            .is_some_and(fonts::Picker::is_open);
        for (index, row) in self.rows.iter_mut().enumerate() {
            if row.key == Some(SettingKey::FontFamily) && font_picker_open {
                continue;
            }
            let value = row
                .key
                .map(|key| document.terminal.value(key))
                .unwrap_or_else(|| document.default_shell.program.clone());
            let current = Self::value(row);
            let composing = COMPOSING.with(Cell::get) == row.input as isize;
            if let Some(color) = themes::COLORS
                .iter()
                .position(|key| Some(*key) == row.key)
                .filter(|_| reset_colors)
            {
                if error.is_none() {
                    if !composing
                        && reset_snapshot
                            .as_ref()
                            .is_some_and(|snapshot| snapshot[color] == current)
                    {
                        Self::set(row, &value);
                        row.due = None;
                    }
                    row.baseline = value;
                    row.error = None;
                }
                continue;
            }
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
                shell_baseline_updated |= row.key.is_none();
                row.error = None;
            } else if !composing
                && current == row.baseline
                && row.due.is_none()
                && row.error.is_none()
            {
                Self::set(row, &value);
                row.baseline = value;
                shell_baseline_updated |= row.key.is_none();
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
            if let Some(error) = draft_error.or(error) {
                self.status(error);
            } else {
                self.default_status();
            }
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
            if let Some(bindings) = self.bindings.as_ref() {
                bindings.enable(self.pending.is_none());
            }
            if let Some(theme) = self.theme_panel.as_ref() {
                theme.enable(self.pending.is_none());
            }
            EnableWindow(self.reset, i32::from(self.pending.is_none()));
            EnableWindow(self.reload, i32::from(self.pending.is_none()));
            EnableWindow(self.focus_color_button, i32::from(self.pending.is_none()));
            if let Some(picker) = self.font_picker.as_ref() {
                EnableWindow(picker.entry, i32::from(self.pending.is_none()));
            }
        }
    }
    pub(super) fn reset_ready(&self) -> bool {
        self.open
            && self.about.is_none()
            && !self
                .font_picker
                .as_ref()
                .is_some_and(fonts::Picker::is_open)
            && self.pending.is_none()
            && COMPOSING.with(Cell::get) == 0
            && !self
                .bindings
                .as_ref()
                .is_some_and(bindings::Bindings::modal)
    }
    pub(super) fn hide(&mut self) {
        self.about.take();
        if let Some(picker) = self.font_picker.as_mut() {
            picker.close();
        }
        if let Some(bindings) = self.bindings.as_mut() {
            bindings.dismiss();
        }
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
    pub(super) fn show_about(&mut self) -> anyhow::Result<()> {
        if self.reset_ready() {
            let id = Uuid::new_v4();
            let panel = editor::ClosePanel::about(self.window, id, self.background)?;
            chrome::window_theme(panel.window(), self.theme);
            self.about = Some((id, panel));
        }
        Ok(())
    }
    pub(super) fn close_about(&mut self, id: Uuid) {
        if self
            .about
            .as_ref()
            .is_some_and(|(current, _)| *current == id)
        {
            self.about.take();
        }
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
                    chrome::Role::Choice {
                        selected: i == self.page,
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
                } else if self.page == 1 {
                    "Terminal and native window colors"
                } else {
                    "Keyboard shortcuts for terminal input"
                })
                .as_ptr(),
            );
            ShowWindow(
                self.heading,
                if self.page == 0 { SW_HIDE } else { SW_SHOWNA },
            );
            place(self.heading, px(20), px(60), client.right - px(40), px(26));
            let viewport_top = px(if self.page == 0 { 64 } else { 92 });
            let viewport_height = (client.bottom - viewport_top - px(98)).max(px(80));
            place(
                self.viewport,
                px(20),
                viewport_top,
                client.right - px(40),
                viewport_height,
            );
            let content_height = match self.page {
                1 => px(904),
                2 => self.bindings.as_ref().map_or(px(562), |bindings| {
                    bindings.content_height(GetDpiForWindow(self.window))
                }),
                _ => px(8 + 46 * (1 + self.rows.iter().filter(|row| row.page == 0).count() as i32)),
            };
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
            ShowWindow(
                self.theme_heading,
                if self.page == 1 { SW_SHOWNA } else { SW_HIDE },
            );
            place(
                self.theme_heading,
                px(8),
                px(8) - offset,
                view.right - px(16),
                px(26),
            );
            for field in self.browser_engine {
                ShowWindow(field, if self.page == 0 { SW_SHOWNA } else { SW_HIDE });
            }
            let mut general_y = 8;
            ShowWindow(
                self.focus_color_button,
                if self.page == 0 { SW_SHOWNA } else { SW_HIDE },
            );
            for row in &self.rows {
                let show =
                    row.page == self.page && (row.page == 0 || row.key == Some(SettingKey::Theme));
                ShowWindow(row.input, if show { SW_SHOWNA } else { SW_HIDE });
                ShowWindow(
                    row.label,
                    if show && !row.is_toggle() {
                        SW_SHOWNA
                    } else {
                        SW_HIDE
                    },
                );
                if show {
                    let top = if row.page == 0 {
                        let top = general_y;
                        general_y += 46;
                        if row.key == Some(SettingKey::FontSize) {
                            place(
                                self.browser_engine[0],
                                px(8),
                                px(general_y) - offset,
                                px(230),
                                px(30),
                            );
                            place(
                                self.browser_engine[1],
                                px(246),
                                px(general_y) - offset,
                                view.right - px(254),
                                px(30),
                            );
                            general_y += 46;
                        }
                        top
                    } else {
                        44
                    };
                    place(row.label, px(8), px(top) - offset, px(230), px(30));
                    if row.is_toggle() {
                        place(
                            row.input,
                            px(8),
                            px(top) - offset,
                            view.right - px(16),
                            px(30),
                        );
                        continue;
                    }
                    place(
                        row.input,
                        px(246),
                        px(top) - offset,
                        view.right
                            - px(
                                if matches!(
                                    row.key,
                                    Some(SettingKey::FontFamily | SettingKey::FocusBorderColor)
                                ) {
                                    342
                                } else if row.key == Some(SettingKey::Theme) {
                                    438
                                } else {
                                    254
                                },
                            ),
                        if row.choices.is_empty() {
                            px(30)
                        } else {
                            px(160)
                        },
                    );
                    if row.key == Some(SettingKey::FontFamily) {
                        if let Some(picker) = &self.font_picker {
                            place(
                                picker.entry,
                                view.right - px(88),
                                px(top) - offset,
                                px(80),
                                px(30),
                            );
                        }
                    }
                    if row.key == Some(SettingKey::FocusBorderColor) {
                        place(
                            self.focus_color_button,
                            view.right - px(88),
                            px(top) - offset,
                            px(80),
                            px(30),
                        );
                    }
                }
            }
            if let Some(picker) = self.font_picker.as_ref() {
                ShowWindow(
                    picker.entry,
                    if self.page == 0 { SW_SHOWNA } else { SW_HIDE },
                );
            }
            if let Some(theme) = self.theme_panel.as_ref() {
                theme.layout(
                    self,
                    self.page == 1,
                    view.right,
                    GetDpiForWindow(self.window),
                    offset,
                );
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
                px(if self.page == 2 { 302 } else { 184 }),
                client.bottom - px(40),
                px(if self.page == 2 { 110 } else { 125 }),
                px(28),
            );
            place(
                self.about_button,
                client.right - px(184),
                client.bottom - px(40),
                px(64),
                px(28),
            );
            place(
                self.close,
                client.right - px(110),
                client.bottom - px(40),
                px(90),
                px(28),
            );
            ShowWindow(self.viewport, SW_SHOWNA);
            ShowWindow(self.reset, if self.page < 2 { SW_SHOWNA } else { SW_HIDE });
            if let Some(bindings) = self.bindings.as_ref() {
                bindings.layout(
                    self.page == 2,
                    view.right,
                    GetDpiForWindow(self.window),
                    offset,
                    client.bottom,
                );
            }
        }
    }
    #[cfg(debug_assertions)]
    pub(in super::super) fn capture_window(&self) -> Option<HWND> {
        self.open.then(|| {
            if let Some((_, about)) = &self.about {
                return about.window();
            }
            self.font_picker
                .as_ref()
                .and_then(fonts::Picker::capture_window)
                .or_else(|| {
                    self.bindings
                        .as_ref()
                        .and_then(bindings::Bindings::capture_window)
                })
                .unwrap_or(self.window)
        })
    }
    pub(super) fn diagnostics(&self) -> Value {
        json!({"browser_engine":{"name":"WebView2","label":self.browser_engine[0] as usize,"value":self.browser_engine[1] as usize},"focus_color_picker":self.focus_color_button as usize,"about_button":self.about_button as usize,"about":self.about.as_ref().map(|(_, panel)|panel.diagnostics()),"window":self.window as usize,"owner":unsafe{GetWindow(self.window,GW_OWNER)} as usize,"open":self.open,"native_visible":unsafe{IsWindowVisible(self.window)}!=0,"modal":false,"page":match self.page {0=>"general",1=>"theme",_=>"keybindings"},"pending":self.pending.is_some(),"auto_apply":true,"queued":self.rows.iter().filter(|row|row.due.is_some()).count(),"composing":COMPOSING.with(Cell::get)!=0,"viewport":self.viewport as usize,"scroll_offset":self.scroll.get(),"error_or_status":Self::text(self.status),"tabs":[{"name":"General","handle":self.tabs[0] as usize},{"name":"Theme","handle":self.tabs[1] as usize},{"name":"Keybindings","handle":self.tabs[2] as usize}],"keybindings":self.bindings.as_ref().map(bindings::Bindings::diagnostics),"font_picker":self.font_picker.as_ref().map(fonts::Picker::diagnostics),"theme_panel":self.theme_panel.as_ref().map(|theme|theme.diagnostics(self)),"reset":self.reset as usize,"reload":self.reload as usize,"close":self.close as usize,"controls":self.rows.iter().map(|row|json!({"key":row.key.map(|key|serde_json::to_value(key).unwrap()).unwrap_or(json!("default_shell")),"label":Self::text(row.label),"input":row.input as usize,"parent":self.viewport as usize,"draft_error":row.error,"page":if row.page==0{"general"}else{"theme"},"value":Self::value(row),"baseline":row.baseline})).collect::<Vec<_>>()})
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        if self
            .about
            .as_ref()
            .is_some_and(|(_, about)| about.handle_message(message))
        {
            return true;
        }
        if self
            .font_picker
            .as_ref()
            .is_some_and(|picker| picker.handle_message(message))
        {
            return true;
        }
        if self
            .bindings
            .as_ref()
            .is_some_and(|bindings| bindings.handle_message(message))
        {
            return true;
        }
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
                !row.choices.is_empty()
                    && !row.is_toggle()
                    && SendMessageW(row.input, CB_GETDROPPEDSTATE, 0, 0) != 0
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
