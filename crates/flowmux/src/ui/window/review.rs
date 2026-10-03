// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

impl WindowController {
    pub(super) async fn open_diff_review(&self) {
        let Some(pane) = self.focused_pane.get() else {
            return;
        };
        if self.pane_registry.borrow().is_ssh_pane(pane) {
            self.clipboard_toast.show_with_message("Diff review requires a local checkout. Open the remote checkout locally to review it.");
            return;
        }
        let Some(root) = self.file_browser_root_for_pane(pane).await else {
            return;
        };
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let review = self
            .reviews
            .borrow_mut()
            .entry(root.clone())
            .or_insert_with(|| crate::ui::review_window::ReviewWindow::new(&self.window, root))
            .clone();
        review.window.present();
    }
}
