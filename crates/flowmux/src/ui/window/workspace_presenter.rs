// SPDX-License-Identifier: GPL-3.0-or-later
//! Authoritative workspace state and its sidebar/pane widget projection.

use super::*;

#[derive(Clone)]
pub struct WorkspacePresenter {
    pub(super) store: StateStore,
    pub(super) sidebar: Sidebar,
    pub(super) stack: gtk::Stack,
    pub(super) surfaces: Rc<RefCell<HashMap<WorkspaceId, gtk::Widget>>>,
    pub(super) pane_registry: Rc<RefCell<PaneRegistry>>,
}

impl WorkspacePresenter {
    /// Keep the authoritative active tab and its widget projection together.
    pub(super) async fn activate_surface(
        &self,
        pane: PaneId,
        surface: SurfaceId,
    ) -> Result<WorkspaceId, String> {
        if !self.pane_registry.borrow().has_surface(pane, surface) {
            return Err(format!("surface is not rendered in pane {pane}: {surface}"));
        }
        crate::ui::workspace_view::materialize_surface(&self.pane_registry, surface)?;
        let workspace = self
            .store
            .set_active_surface(pane, surface)
            .await
            .ok_or_else(|| format!("surface not found in pane {pane}: {surface}"))?;
        self.pane_registry
            .borrow_mut()
            .activate_surface(pane, surface);
        Ok(workspace)
    }

    pub(super) fn new(
        store: StateStore,
        sidebar: Sidebar,
        stack: gtk::Stack,
        surfaces: Rc<RefCell<HashMap<WorkspaceId, gtk::Widget>>>,
        pane_registry: Rc<RefCell<PaneRegistry>>,
    ) -> Self {
        Self {
            store,
            sidebar,
            stack,
            surfaces,
            pane_registry,
        }
    }
}
