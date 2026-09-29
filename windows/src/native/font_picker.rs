// SPDX-License-Identifier: GPL-3.0-or-later
//! Installed-font search is a draft-only modal; selection reuses the Options writer.
use super::*;
use std::{cell::RefCell, collections::HashMap, sync::mpsc};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, IsWindowEnabled};
#[path = "font_catalog.rs"]
mod catalog;

pub(super) const TIMER: usize = 8;
#[derive(Clone, Copy)]
pub(crate) enum Signal {
    Open,
    Poll,
    Changed(Uuid),
    Selected(Uuid),
    Choose(Uuid),
    Cancel(Uuid),
    Layout(Uuid),
}
#[derive(Clone, Copy)]
struct Route {
    id: Uuid,
    composing: bool,
    settling: bool,
}
thread_local! { static ROUTES: RefCell<HashMap<isize, Route>> = RefCell::new(HashMap::new()); }
fn route(window: HWND) -> Option<Route> {
    ROUTES.with(|routes| routes.borrow().get(&(window as isize)).copied())
}
fn send(signal: Signal) {
    super::emit(UiAction::FontPicker(signal));
}
unsafe extern "system" fn edit_proc(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    let parent = GetParent(window);
    ROUTES.with(|routes| {
        if let Some(r) = routes.borrow_mut().get_mut(&(parent as isize)) {
            match message {
                WM_IME_STARTCOMPOSITION => {
                    r.composing = true;
                    r.settling = true;
                }
                WM_IME_ENDCOMPOSITION => {
                    r.composing = false;
                    r.settling = true;
                }
                WM_KEYDOWN if w == 229 => r.settling = true,
                WM_KEYUP if !matches!(w, 0x10..=0x12 | 0x5b | 0x5c | 0xa0..=0xa5) => {
                    r.settling = false;
                }
                _ => {}
            }
        }
    });
    let result = DefSubclassProc(window, message, w, l);
    if message == WM_IME_ENDCOMPOSITION {
        if let Some(r) = route(parent) {
            send(Signal::Changed(r.id));
        }
    }
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(window, Some(edit_proc), id);
    }
    result
}
unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Some(r) = route(window) {
        let signal = match message {
            WM_CLOSE => Some(Signal::Cancel(r.id)),
            WM_SIZE => Some(Signal::Layout(r.id)),
            WM_COMMAND => match (w & 0xffff, (w >> 16) as u32) {
                (100, EN_CHANGE) => Some(Signal::Changed(r.id)),
                (101, LBN_SELCHANGE) => Some(Signal::Selected(r.id)),
                (101, LBN_DBLCLK) | (1, BN_CLICKED) => Some(Signal::Choose(r.id)),
                (2, BN_CLICKED) => Some(Signal::Cancel(r.id)),
                _ => None,
            },
            WM_DPICHANGED if l != 0 => {
                let rect = &*(l as *const RECT);
                SetWindowPos(
                    window,
                    std::ptr::null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                Some(Signal::Layout(r.id))
            }
            WM_GETMINMAXINFO if l != 0 => {
                let info = &mut *(l as *mut MINMAXINFO);
                let dpi = GetDpiForWindow(window).max(96) as i32;
                info.ptMinTrackSize.x = 460 * dpi / 96;
                info.ptMinTrackSize.y = 420 * dpi / 96;
                return 0;
            }
            WM_NCDESTROY => {
                ROUTES.with(|routes| routes.borrow_mut().remove(&(window as isize)));
                None
            }
            _ => None,
        };
        if let Some(signal) = signal {
            send(signal);
            return 0;
        }
    }
    if let Some(result) = chrome::message(window, message, w, l) {
        return result;
    }
    DefWindowProcW(window, message, w, l)
}
struct Choice {
    label: String,
    value: Option<String>,
    family: Option<String>,
    kind: &'static str,
}
struct Popup {
    id: Uuid,
    window: HWND,
    search: HWND,
    list: HWND,
    hint: HWND,
    status: HWND,
    choose: HWND,
    cancel: HWND,
    disabled: Vec<HWND>,
    previous: HWND,
    background: bool,
    source_raw: String,
    choices: Vec<Choice>,
    filtered: Vec<usize>,
    error: Option<String>,
}
impl Drop for Popup {
    fn drop(&mut self) {
        unsafe {
            let restore = GetForegroundWindow() == self.window;
            for owner in &self.disabled {
                if IsWindow(*owner) != 0 {
                    EnableWindow(*owner, 1);
                }
            }
            ROUTES.with(|routes| routes.borrow_mut().remove(&(self.window as isize)));
            DestroyWindow(self.window);
            if !self.background
                && restore
                && !self.previous.is_null()
                && IsWindow(self.previous) != 0
                && IsWindowEnabled(self.previous) != 0
                && IsWindowVisible(self.previous) != 0
            {
                SetFocus(self.previous);
            }
        }
    }
}
impl Popup {
    fn new(
        owner: HWND,
        raw: &str,
        background: bool,
        theme: crate::settings::Theme,
    ) -> anyhow::Result<Self> {
        unsafe {
            let class = wide("flowmux.windows.font.picker");
            let instance = GetModuleHandleW(std::ptr::null());
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register font picker"
            );
            let dpi = GetDpiForWindow(owner).max(96) as i32;
            let mut bounds = RECT::default();
            GetWindowRect(owner, &mut bounds);
            let width = 460 * dpi / 96;
            let height = 420 * dpi / 96;
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Choose terminal font").as_ptr(),
                WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN,
                bounds.left + (bounds.right - bounds.left - width) / 2,
                bounds.top + (bounds.bottom - bounds.top - height) / 2,
                width,
                height,
                owner,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create font picker");
            let mut popup = Self {
                id: Uuid::new_v4(),
                window,
                search: std::ptr::null_mut(),
                list: std::ptr::null_mut(),
                hint: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                choose: std::ptr::null_mut(),
                cancel: std::ptr::null_mut(),
                disabled: Vec::new(),
                previous: if background {
                    std::ptr::null_mut()
                } else {
                    GetFocus()
                },
                background,
                source_raw: raw.to_owned(),
                choices: Vec::new(),
                filtered: Vec::new(),
                error: None,
            };
            popup.hint = popup.child(
                "STATIC",
                "Search installed coding fonts. Custom fallback text stays available in Options.",
                102,
                SS_NOPREFIX,
            )?;
            popup.search = popup.child(
                "EDIT",
                "",
                100,
                WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
            )?;
            popup.list = popup.child(
                "LISTBOX",
                "",
                101,
                WS_TABSTOP
                    | WS_BORDER
                    | WS_VSCROLL
                    | LBS_NOTIFY as u32
                    | LBS_NOINTEGRALHEIGHT as u32,
            )?;
            popup.status = popup.child("STATIC", "", 103, SS_NOPREFIX)?;
            popup.choose = popup.child("BUTTON", "Use font", 1, WS_TABSTOP)?;
            popup.cancel = popup.child("BUTTON", "Cancel", 2, WS_TABSTOP)?;
            ROUTES.with(|routes| {
                routes.borrow_mut().insert(
                    window as isize,
                    Route {
                        id: popup.id,
                        composing: false,
                        settling: false,
                    },
                )
            });
            SendMessageW(popup.search, EM_LIMITTEXT, 256, 0);
            checked(SetWindowSubclass(popup.search, Some(edit_proc), 100, 0))?;
            let mut target = owner;
            for _ in 0..8 {
                if target.is_null() {
                    break;
                }
                if IsWindowEnabled(target) != 0 {
                    EnableWindow(target, 0);
                    popup.disabled.push(target);
                }
                target = GetWindow(target, GW_OWNER);
            }
            chrome::window_theme(window, theme);
            popup.layout();
            if !background {
                ShowWindow(window, SW_SHOW);
                SetFocus(popup.search);
            }
            Ok(popup)
        }
    }
    fn child(&self, class: &str, text: &str, id: usize, style: u32) -> anyhow::Result<HWND> {
        unsafe {
            let style = if class == "BUTTON" {
                (style & !0xf) | BS_OWNERDRAW as u32
            } else {
                style
            };
            let window = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                1,
                1,
                self.window,
                id as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create font picker control");
            if class == "BUTTON" {
                chrome::register_button(window, chrome::Role::Button);
            } else {
                chrome::register_control(
                    window,
                    match class {
                        "EDIT" => chrome::ControlRole::Edit,
                        "LISTBOX" => chrome::ControlRole::Listbox,
                        _ => chrome::ControlRole::Caption,
                    },
                );
            }
            Ok(window)
        }
    }
    fn composing(&self) -> bool {
        route(self.window).is_some_and(|r| r.composing)
    }
    fn selected(&self) -> Option<usize> {
        let index = unsafe { SendMessageW(self.list, LB_GETCURSEL, 0, 0) };
        usize::try_from(index)
            .ok()
            .and_then(|index| self.filtered.get(index).copied())
    }
    fn filter(&mut self) {
        let previous = self.selected();
        // Reuse caption normalization for comparison copies; keep input and values raw.
        let search_key = |text: &str| {
            let raw: Vec<_> = text.encode_utf16().collect();
            String::from_utf16_lossy(&chrome::caption_for_paint(&raw)).to_lowercase()
        };
        let query = search_key(&Panel::text(self.search));
        self.filtered = self
            .choices
            .iter()
            .enumerate()
            .filter(|(_, choice)| search_key(&choice.label).contains(&query))
            .map(|(index, _)| index)
            .collect();
        unsafe {
            SendMessageW(self.list, LB_RESETCONTENT, 0, 0);
            for index in &self.filtered {
                SendMessageW(
                    self.list,
                    LB_ADDSTRING,
                    0,
                    wide(&self.choices[*index].label).as_ptr() as LPARAM,
                );
            }
            let selected = previous
                .and_then(|index| self.filtered.iter().position(|i| *i == index))
                .unwrap_or(0);
            if !self.filtered.is_empty() {
                SendMessageW(self.list, LB_SETCURSEL, selected, 0);
            }
            EnableWindow(
                self.choose,
                i32::from(!self.filtered.is_empty() && !self.composing()),
            );
        }
    }
    fn choices(&mut self, catalog: Option<&catalog::Catalog>) {
        self.choices = vec![
            Choice {
                label: format!("Current custom: {}", self.source_raw),
                value: Some(self.source_raw.clone()),
                family: None,
                kind: "current",
            },
            Choice {
                label: "Windows default".into(),
                value: Some(crate::settings::TerminalSettings::default().font_family),
                family: None,
                kind: "default",
            },
        ];
        if let Some(catalog) = catalog {
            for family in &catalog.families {
                self.choices.push(Choice {
                    label: family.clone(),
                    value: catalog::replace_primary(&self.source_raw, family).ok(),
                    family: Some(family.clone()),
                    kind: "installed",
                });
            }
        }
        self.filter();
    }
    fn status(&self, text: &str) {
        unsafe {
            SetWindowTextW(self.status, wide(text).as_ptr());
        }
    }
    fn layout(&self) {
        unsafe {
            let mut rect = RECT::default();
            GetClientRect(self.window, &mut rect);
            let dpi = GetDpiForWindow(self.window).max(96) as i32;
            let px = |n| n * dpi / 96;
            let place = |window, x, y, width: i32, height: i32| {
                SetWindowPos(
                    window,
                    std::ptr::null_mut(),
                    x,
                    y,
                    width.max(1),
                    height.max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            };
            place(self.hint, px(12), px(10), rect.right - px(24), px(38));
            place(self.search, px(12), px(52), rect.right - px(24), px(28));
            place(
                self.list,
                px(12),
                px(88),
                rect.right - px(24),
                rect.bottom - px(186),
            );
            place(
                self.status,
                px(12),
                rect.bottom - px(90),
                rect.right - px(24),
                px(44),
            );
            place(
                self.choose,
                rect.right - px(190),
                rect.bottom - px(38),
                px(90),
                px(28),
            );
            place(
                self.cancel,
                rect.right - px(92),
                rect.bottom - px(38),
                px(80),
                px(28),
            );
        }
    }
    fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            if message.hwnd != self.window && IsChild(self.window, message.hwnd) == 0 {
                return false;
            }
            if route(self.window).is_some_and(|r| r.composing || r.settling)
                || message.wParam == 229
            {
                return false;
            }
            if message.message == WM_KEYDOWN {
                if message.wParam == 27 {
                    send(Signal::Cancel(self.id));
                    return true;
                }
                if message.wParam == 13 {
                    if message.hwnd == self.cancel {
                        send(Signal::Cancel(self.id));
                    } else if IsWindowEnabled(self.choose) != 0 {
                        send(Signal::Choose(self.id));
                    }
                    return true;
                }
            }
            IsDialogMessageW(self.window, message) != 0
        }
    }
}
pub(super) struct Picker {
    owner: HWND,
    pub(super) entry: HWND,
    popup: Option<Popup>,
    receiver: Option<mpsc::Receiver<Result<catalog::Catalog, String>>>,
    started: Option<Instant>,
    catalog: Option<catalog::Catalog>,
    state: &'static str,
    catalog_error: Option<String>,
}
impl Drop for Picker {
    fn drop(&mut self) {
        self.close();
        unsafe {
            KillTimer(self.owner, TIMER);
        }
    }
}
impl Picker {
    pub(super) fn new(owner: HWND, entry: HWND) -> Self {
        Self {
            owner,
            entry,
            popup: None,
            receiver: None,
            started: None,
            catalog: None,
            state: "idle",
            catalog_error: None,
        }
    }
    pub(super) fn open(
        &mut self,
        raw: &str,
        background: bool,
        theme: crate::settings::Theme,
    ) -> anyhow::Result<()> {
        if self.popup.is_some() {
            return Ok(());
        }
        let popup = Popup::new(self.owner, raw, background, theme)?;
        self.popup = Some(popup);
        if self.state == "idle" {
            let (tx, rx) = mpsc::sync_channel(1);
            self.state = "loading";
            self.started = Some(Instant::now());
            match std::thread::Builder::new()
                .name("flowmux-font-catalog".into())
                .spawn(move || {
                    let _ = tx.send(catalog::enumerate());
                }) {
                Ok(_) => {
                    self.receiver = Some(rx);
                    if unsafe { SetTimer(self.owner, TIMER, 50, None) } == 0 {
                        self.receiver = None;
                        self.state = "error";
                        self.catalog_error = Some(
                            "Cannot schedule font lookup. Use the original Options field.".into(),
                        );
                    }
                }
                Err(error) => {
                    self.state = "error";
                    self.catalog_error = Some(error.to_string());
                }
            }
        }
        self.refresh();
        Ok(())
    }
    fn refresh(&mut self) {
        if let Some(popup) = self.popup.as_mut() {
            popup.choices(self.catalog.as_ref());
            self.show_status();
        }
    }
    fn show_status(&self) {
        if let Some(popup) = self.popup.as_ref() {
            popup.status(popup.error.as_deref().or(self.catalog_error.as_deref()).unwrap_or(match self.state {
                "loading" => "Reading installed coding fonts… Search does not save settings.",
                "timeout" => "Font lookup timed out. Current custom and Windows default remain available.",
                "truncated" => "Font list was bounded. Use the original Options field for other fonts.",
                _ if popup.filtered.is_empty() => "No matching fonts.",
                _ => "Use font applies this choice. Cancel keeps the current setting.",
            }));
        }
    }
    pub(super) fn poll(&mut self) {
        if self.state != "loading" {
            return;
        }
        // Check the UI deadline before accepting an already queued late result.
        // Dropping the receiver cancels delivery without joining a stalled GDI call.
        if self
            .started
            .is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
        {
            self.state = "timeout";
            self.receiver = None;
            unsafe {
                KillTimer(self.owner, TIMER);
            }
            self.refresh();
            return;
        }
        let result = self.receiver.as_ref().map(mpsc::Receiver::try_recv);
        match result {
            Some(Ok(Ok(catalog))) => {
                self.state = if catalog.truncated {
                    "truncated"
                } else {
                    "ready"
                };
                self.catalog = Some(catalog);
            }
            Some(Ok(Err(error))) => {
                self.state = "error";
                self.catalog_error = Some(error);
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.state = "error";
                self.catalog_error = Some("Font lookup ended without a result.".into());
            }
            _ => return,
        }
        self.receiver = None;
        unsafe {
            KillTimer(self.owner, TIMER);
        }
        self.refresh();
    }
    pub(super) fn signal(&mut self, signal: Signal) -> Option<Result<String, String>> {
        if matches!(signal, Signal::Poll) {
            self.poll();
            return None;
        }
        let popup = self.popup.as_mut()?;
        match signal {
            Signal::Cancel(id) if id == popup.id => {
                self.close();
            }
            Signal::Changed(id) if id == popup.id => {
                if !popup.composing() {
                    popup.filter();
                }
                self.show_status();
            }
            Signal::Selected(id) if id == popup.id => {
                popup.error = None;
                self.show_status();
            }
            Signal::Layout(id) if id == popup.id => popup.layout(),
            Signal::Choose(id) if id == popup.id && !popup.composing() => {
                let choice = popup.selected().and_then(|index| popup.choices.get(index));
                return choice.map(|choice| choice.value.clone().ok_or_else(|| "Cannot replace this custom font expression; edit the original Options field.".into()));
            }
            _ => {}
        }
        None
    }
    pub(super) fn error(&mut self, text: String) {
        if let Some(popup) = self.popup.as_mut() {
            popup.error = Some(text);
        }
        self.show_status();
    }
    pub(super) fn source_raw(&self) -> Option<&str> {
        self.popup.as_ref().map(|popup| popup.source_raw.as_str())
    }
    pub(super) fn is_open(&self) -> bool {
        self.popup.is_some()
    }
    pub(super) fn close(&mut self) {
        self.popup.take();
    }
    pub(super) fn theme(&self, theme: crate::settings::Theme) {
        if let Some(popup) = self.popup.as_ref() {
            chrome::window_theme(popup.window, theme);
        }
    }
    pub(super) fn focus(&self) {
        if let Some(popup) = self.popup.as_ref().filter(|popup| !popup.background) {
            unsafe {
                SetFocus(popup.search);
            }
        }
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        self.popup
            .as_ref()
            .is_some_and(|popup| popup.handle_message(message))
    }
    #[cfg(debug_assertions)]
    pub(super) fn capture_window(&self) -> Option<HWND> {
        self.popup.as_ref().map(|popup| popup.window)
    }
    pub(super) fn diagnostics(&self) -> Value {
        let popup = self.popup.as_ref();
        json!({"entrybutton":self.entry as usize,"open":popup.is_some(),
            "window":popup.map(|p|p.window as usize),"owner":self.owner as usize,
            "search":popup.map(|p|p.search as usize),"list":popup.map(|p|p.list as usize),
            "choose":popup.map(|p|p.choose as usize),"cancel":popup.map(|p|p.cancel as usize),
            "catalog_state":self.state,"catalog":self.catalog.as_ref().map(|c|json!({"visited":c.visited,"elapsed_ms":c.elapsed_ms,"truncated":c.truncated,"families":c.families})),
            "choices":popup.map(|p|p.choices.iter().map(|c|json!({"label":c.label,"value":c.value,"family":c.family,"kind":c.kind})).collect::<Vec<_>>()),
            "filtered":popup.map(|p|&p.filtered),"selected":popup.and_then(Popup::selected),
            "query":popup.map(|p|Panel::text(p.search)),"source_raw":popup.map(|p|&p.source_raw),
            "composing":popup.is_some_and(Popup::composing),"error":popup.and_then(|p|p.error.as_deref()).or(self.catalog_error.as_deref())})
    }
}
