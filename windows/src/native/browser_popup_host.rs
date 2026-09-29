// SPDX-License-Identifier: GPL-3.0-or-later
//! Commit deferred popups as tabs or independent children of detached browsers.
use super::*;
use crate::browser_popup as domain;
use flowmux_core::PaneSurface;

impl App {
    pub(crate) fn browser_popup_status(&self) -> Value {
        let mut status = self.browser_popups.status();
        status["active"] = json!(self
            .browsers
            .values()
            .filter(|b| b.popup_opener.is_some())
            .count());
        status["limit"] = json!(domain::MAX_POPUP_TABS);
        status["pending_limit"] = json!(popup::MAX_PENDING);
        status
    }
    pub(super) fn browser_popup_dispatch(&mut self) {
        for request in self.browser_popups.drain() {
            let source = request.surface;
            if let Err(error) = self.browser_popup_open(request) {
                let error = format!("{error:#}");
                self.browser_popups.rejected(&error);
                if let Some(browser) = self.browsers.get_mut(&source) {
                    browser.error = Some(error);
                }
            }
        }
    }
    fn browser_popup_open(&mut self, mut request: popup::Request) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.close_request.is_none() && !self.close_accepted && !self.closing,
            "window is closing"
        );
        anyhow::ensure!(
            request.valid(),
            "popup opener changed, closed or request expired"
        );
        domain::admit(
            self.browsers
                .values()
                .filter(|b| b.popup_opener.is_some())
                .count(),
        )?;
        let (index, _, _) = self
            .locate(request.surface)
            .context("popup opener no longer exists")?;
        let opener = self
            .browsers
            .get(&request.surface)
            .context("popup opener is not a browser")?;
        anyhow::ensure!(
            opener.visible && !opener.native_closed.get(),
            "popup opener is hidden or closed"
        );
        anyhow::ensure!(
            opener.preview_binding.is_none() || opener.preview_generation.is_some(),
            "SSH preview has expired"
        );
        let persistent = opener.persistent;
        let preview = opener
            .preview_binding
            .clone()
            .map(|binding| (binding, opener.preview_generation));
        let separate = if let Some(window) = self.detached.get(&request.surface) {
            anyhow::ensure!(
                window.workspace == self.workspaces[index].id,
                "popup opener window changed"
            );
            true
        } else {
            anyhow::ensure!(
                index == self.active_workspace,
                "popup opener workspace is hidden"
            );
            false
        };
        let previous = self.workspaces.clone();
        let mut candidate = previous.clone();
        let opened = domain::open(&mut candidate[index], request.surface, request.uri.clone())?;
        if let Some((binding, _)) = &preview {
            candidate[index].root.set_surface_browser_url(
                opened.pane,
                opened.surface,
                binding.clone(),
            );
        }
        let destination = if separate {
            Some(model::detach_surface(&mut candidate, opened.surface)?)
        } else {
            None
        };
        let child = Browser::new_in_environment(
            self,
            opened.surface,
            Some(request.environment()),
            persistent,
        );
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                self.browser_cancel(opened.surface, "popup construction failed");
                return Err(error.context("cannot create popup browser tab"));
            }
        };
        // Construction pumps native callbacks. Do not attach after a navigation,
        // close or visibility transition which happened during that pump.
        if !request.valid() {
            self.browser_cancel(opened.surface, "popup opener changed during construction");
            drop(child);
            anyhow::bail!("popup opener changed during browser construction");
        }
        child.popup_opener = Some(request.surface);
        if let Some((binding, generation)) = preview {
            child.inherit_preview(Some(binding), generation);
        }
        child.popup_user_initiated = Some(request.user_initiated);
        child.url = opened.url;
        let core = match unsafe { child.view.controller().CoreWebView2() } {
            Ok(core) => core,
            Err(error) => {
                self.browser_cancel(opened.surface, "popup controller became unavailable");
                return Err(error.into());
            }
        };
        // Keep the frame local and hidden until SetNewWindow completes. Its
        // holder is moved intact; the new browser still has the opener's native
        // environment and has not been independently navigated.
        let window = if let Some(destination) = destination {
            let workspace = &candidate[destination];
            let sidebar_width = self
                .detached
                .get(&request.surface)
                .map_or(self.sidebar_width_dip, detached::Window::sidebar_width_dip);
            let window = match detached::Window::new(
                opened.surface,
                workspace.id,
                "Browser",
                sidebar_width,
                self.background_test,
            ) {
                Ok(window) => window,
                Err(error) => {
                    self.browser_cancel(opened.surface, "popup window construction failed");
                    drop(core);
                    drop(child);
                    return Err(error);
                }
            };
            super::super::chrome::window_theme(window.window, self.settings.terminal.theme);
            window.caption("Browser", super::super::chrome::SurfaceIcon::Browser);
            if let Err(error) = child.holder.reparent(window.window) {
                self.browser_cancel(opened.surface, "popup window attachment failed");
                drop(core);
                drop(child);
                drop(window);
                return Err(error);
            }
            Some(window)
        } else {
            None
        };
        if !request.valid() {
            self.browser_cancel(
                opened.surface,
                "popup opener changed during window construction",
            );
            drop(core);
            drop(child); // WebView and holder must precede their frame on every path.
            drop(window);
            anyhow::bail!("popup opener changed during window construction");
        }
        // Install the model before completing the deferral: completion may cause
        // navigation or window.close callbacks. There is no separate Navigate.
        self.workspaces = candidate;
        self.browsers.insert(opened.surface, child);
        if let Some(window) = window {
            self.detached.insert(opened.surface, window);
        }
        if let Err(error) = request.attach(&core) {
            drop(request);
            drop(core);
            self.workspaces = previous;
            self.browser_cancel(opened.surface, "native popup attachment failed");
            self.browsers.remove(&opened.surface);
            self.detached.remove(&opened.surface);
            return Err(error.context("cannot attach native popup"));
        }
        drop(core);
        drop(request);
        self.browser_popups.opened();
        self.zoomed = None;
        if separate {
            self.detached_focus = Some(opened.surface);
        }
        // Once attached, a WindowProxy may already refer to the child. Retain it
        // if a layout update fails instead of undoing a completed native action.
        let layout = if separate {
            let layout = self.rebuild_without_focus();
            self.detached[&opened.surface].show(self.background_test);
            layout.and_then(|()| {
                self.select(opened.surface)?;
                self.focus_active()
            })
        } else {
            self.rebuild()
        };
        if let Err(error) = layout {
            report(&format!("popup tab layout: {error:#}"));
            if let Some(browser) = self.browsers.get_mut(&opened.surface) {
                browser.error = Some(format!("Popup opened; layout update failed: {error}"));
            }
        }
        Ok(())
    }
    pub(in crate::native::host) fn browser_resume_closed(&mut self) {
        // WindowCloseRequested fires once. Files completion retries any close
        // that it deferred, validating the instance and current layout again.
        let closed: Vec<_> = self
            .browsers
            .iter()
            .filter(|(_, browser)| browser.native_closed.get())
            .map(|(id, browser)| (*id, browser.instance))
            .collect();
        for (id, instance) in closed {
            if let Err(error) = self.browser_popup_close(id, instance) {
                report(&format!("browser close after Files completion: {error:#}"));
            }
        }
    }
    pub(super) fn browser_popup_close(
        &mut self,
        id: SurfaceId,
        instance: Uuid,
    ) -> anyhow::Result<()> {
        if self
            .browsers
            .get(&id)
            .is_none_or(|b| b.instance != instance || !b.native_closed.get())
        {
            return Ok(());
        }
        if self.detached.contains_key(&id) {
            return self.close_detached(id);
        }
        let (index, pane, _) = self
            .locate(id)
            .context("closed browser is missing from layout")?;
        let mut candidate = self.workspaces[index].clone();
        let final_tab = candidate
            .leaves()
            .iter()
            .map(|(_, _, tabs)| tabs.len())
            .sum::<usize>()
            == 1;
        // Wry has already destroyed this view's container HWND. Even a refused
        // final-tab close therefore needs a fresh view and a fresh surface ID.
        // A new ID also prevents old script/download callbacks reaching it.
        let replacement = if final_tab {
            let tab = PaneSurface::browser("Browser", "about:blank".into());
            let replacement = tab.id;
            candidate
                .root
                .add_surface_to_leaf(pane, tab)
                .context("final browser pane disappeared")?;
            domain::close(&mut candidate, id)?;
            Some(replacement)
        } else {
            domain::close(&mut candidate, id)?;
            None
        };
        let focused = index == self.active_workspace && self.current_surface() == Some(id);
        self.workspaces[index] = candidate;
        self.remove_surface(id);
        if self.zoomed == Some(pane)
            && self.workspaces[index]
                .root
                .find_leaf_content(pane)
                .is_none()
        {
            self.zoomed = None;
        }
        if let Some(replacement) = replacement {
            // Drop the destroyed controller before allocating another HWND.
            // A failed creation is reported; a later explicit structural action
            // can retry the model's blank browser, without an automatic loop.
            self.add_browser_view(replacement, "about:blank".into())?;
        }
        if focused {
            self.rebuild()
        } else {
            self.rebuild_without_focus()
        }
    }
}
