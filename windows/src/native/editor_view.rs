// SPDX-License-Identifier: GPL-3.0-or-later
//! Isolated Windows editor WebView; no terminal bridge or browser popup policy.
use crate::{editor_assets::EditorAssets, model};
use anyhow::Context;
use flowmux_core::SurfaceId;
use raw_window_handle::{HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use std::{cell::Cell, num::NonZeroIsize};
use uuid::Uuid;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetGUIThreadInfo, GetWindowThreadProcessId, IsChild, GUITHREADINFO,
};
use wry::{WebContext, WebView, WebViewBuilder, WebViewExtWindows};

struct Parent(HWND);
impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        let handle = Win32WindowHandle::new(
            NonZeroIsize::new(self.0 as isize)
                .ok_or(raw_window_handle::HandleError::Unavailable)?,
        );
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
    }
}

pub struct View {
    pub view: WebView,
    pub url: String,
    pub credential: String,
    /// Logical visibility; background verification never shows the native view.
    pub visible: bool,
    background: bool,
    window: HWND,
    bounds: Option<model::Rect>,
}

impl View {
    pub fn new(
        window: HWND,
        surface: SurfaceId,
        context: &mut WebContext,
        assets: &EditorAssets,
        background: bool,
        emit: impl Fn(String, String) + 'static,
    ) -> anyhow::Result<Self> {
        let url = assets.url(surface);
        let credential = Uuid::new_v4().to_string();
        let init = initialization(&url, surface, &credential, background)?;
        let navigation_url = url.clone();
        let message_url = url.clone();
        let navigated = Cell::new(false);
        let view = WebViewBuilder::new_with_web_context(context)
            .with_visible(false)
            .with_focused(false)
            .with_devtools(false)
            .with_hotkeys_zoom(false)
            .with_clipboard(!background)
            .with_background_color((23, 25, 31, 255))
            .with_initialization_script_for_main_only(init, true)
            .with_navigation_handler(move |destination| {
                // A same-URL reload would recreate an empty frontend using the
                // native instance's credentials while its worker retains data.
                // Permit only the initial entry; no automatic reload/recovery.
                destination == navigation_url && !navigated.replace(true)
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_download_started_handler(|_, _| false)
            .with_permission_handler(|_| wry::PermissionResponse::Deny)
            .with_ipc_handler(move |request| {
                // WebView2 supplies the source URI. Never accept another local
                // asset or editor instance merely because it shares the origin.
                if request.uri().to_string() == message_url
                    && request.body().len() <= flowmux_editor::MAX_BRIDGE_MESSAGE_BYTES + 4096
                {
                    emit(request.uri().to_string(), request.body().clone());
                }
            })
            .with_url(&url)
            .build_as_child(&Parent(window))
            .context("cannot create editor WebView2 view")?;
        unsafe {
            let settings = view.controller().CoreWebView2()?.Settings()?;
            settings.SetAreHostObjectsAllowed(false)?;
            settings.SetAreDefaultScriptDialogsEnabled(false)?;
        }
        Ok(Self {
            view,
            url,
            credential,
            visible: false,
            background,
            window,
            bounds: None,
        })
    }

    pub fn layout(&mut self, area: Option<model::Rect>) -> anyhow::Result<()> {
        if let Some(area) = area.filter(|area| self.bounds != Some(*area)) {
            self.view.set_bounds(wry::Rect {
                position: wry::dpi::PhysicalPosition::new(area.x, area.y).into(),
                size: wry::dpi::PhysicalSize::new(
                    area.width.max(1) as u32,
                    area.height.max(1) as u32,
                )
                .into(),
            })?;
            self.bounds = Some(area);
        }
        let show = area.is_some();
        if self.visible != show {
            if !self.background {
                self.view.set_visible(show)?;
            }
            self.visible = show;
        }
        Ok(())
    }

    pub fn focus(&self) -> anyhow::Result<()> {
        if self.visible && !self.background {
            let mut info = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..GUITHREADINFO::default()
            };
            let child = self.view.hwnd().0;
            let focused = unsafe {
                let thread = GetWindowThreadProcessId(self.window, std::ptr::null_mut());
                GetGUIThreadInfo(thread, &mut info) != 0
                    && (info.hwndFocus == child || IsChild(child, info.hwndFocus) != 0)
            };
            // Repeating WebView2 MoveFocus can interrupt an already focused
            // Monaco textarea's live composition, even during ordinary rebuilds.
            if !focused {
                self.view.focus()?;
            }
        }
        Ok(())
    }
}

fn initialization(
    url: &str,
    surface: SurfaceId,
    credential: &str,
    background: bool,
) -> anyhow::Result<String> {
    let configuration = serde_json::to_string(&serde_json::json!({
        "url": url, "signal_id": surface.0, "credential": credential, "background": background,
    }))?;
    Ok(format!(
        "({})({configuration});",
        include_str!("../../editor/initialize.js")
    ))
}
