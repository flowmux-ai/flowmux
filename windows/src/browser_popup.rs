// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure routing for browser new-window requests into their source pane's tabs.
use crate::{browser, model::Workspace};
use anyhow::Context;
use flowmux_core::{
    CloseSurfaceOutcome, PaneId, PaneSurface, RemoveOutcome, SurfaceId, SurfaceKind,
};

pub const MAX_POPUP_TABS: usize = 16;

/// The native host counts its live and reserved popup tabs before admitting one.
pub fn admit(open_popup_tabs: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        open_popup_tabs < MAX_POPUP_TABS,
        "browser popup tab limit reached ({MAX_POPUP_TABS})"
    );
    Ok(())
}

#[derive(Debug)]
pub struct Opened {
    pub pane: PaneId,
    pub surface: SurfaceId,
    pub url: String,
}

/// Append to a candidate workspace after validating the original browser source.
/// The host checks its foreground workspace and admission count, creates the
/// native WebView, then commits this candidate. The URL is model metadata;
/// supplying a WebView to the opener must not separately navigate that WebView.
pub fn open(workspace: &mut Workspace, source: SurfaceId, url: String) -> anyhow::Result<Opened> {
    let (pane, active, is_browser) = workspace
        .leaves()
        .into_iter()
        .find_map(|(pane, active, tabs)| {
            tabs.into_iter().find(|tab| tab.id == source).map(|tab| {
                (
                    pane,
                    active,
                    matches!(tab.kind, SurfaceKind::Browser { .. }),
                )
            })
        })
        .context("popup source browser tab no longer exists")?;
    anyhow::ensure!(is_browser, "popup source must be a browser tab");
    anyhow::ensure!(active == source, "popup source browser tab is not active");
    let url = browser::url(&url)?;
    let tab = PaneSurface::browser("Browser", url.clone());
    let surface = tab.id;
    workspace
        .root
        .add_surface_to_leaf(pane, tab)
        .context("popup source pane no longer exists")?;
    workspace.focused = pane;
    Ok(Opened { pane, surface, url })
}

/// Remove only this browser tab; the native host verifies popup ownership first.
/// Closing an inactive tab preserves the active tab and any surviving focused pane.
pub fn close(workspace: &mut Workspace, surface: SurfaceId) -> anyhow::Result<()> {
    let leaves = workspace.leaves();
    let (pane, tab_count, is_browser) = leaves
        .iter()
        .find_map(|(pane, _, tabs)| {
            tabs.iter().find(|tab| tab.id == surface).map(|tab| {
                (
                    *pane,
                    tabs.len(),
                    matches!(tab.kind, SurfaceKind::Browser { .. }),
                )
            })
        })
        .context("popup browser tab no longer exists")?;
    anyhow::ensure!(is_browser, "popup close requires a browser tab");
    anyhow::ensure!(
        leaves.len() > 1 || tab_count > 1,
        "cannot close the final tab in a workspace"
    );

    let mut root = workspace.root.clone();
    let empty = match root.close_surface_in_leaf(pane, surface) {
        CloseSurfaceOutcome::SurfaceRemoved => false,
        CloseSurfaceOutcome::LastSurfaceRemoved => true,
        CloseSurfaceOutcome::NotFound => anyhow::bail!("popup browser tab no longer exists"),
    };
    if empty {
        root = match root.remove_leaf(pane) {
            RemoveOutcome::Replaced(root) => root,
            RemoveOutcome::EntirelyRemoved => {
                anyhow::bail!("cannot close the final tab in a workspace")
            }
            RemoveOutcome::NotFound(_) => anyhow::bail!("popup source pane no longer exists"),
        };
    }
    let focused = if empty && workspace.focused == pane {
        root.first_leaf_id()
            .context("workspace has no remaining pane")?
    } else {
        workspace.focused
    };
    workspace.root = root;
    workspace.focused = focused;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_core::{PaneContent, SplitDirection};

    fn browser_workspace() -> (Workspace, PaneId, SurfaceId, SurfaceId) {
        let mut workspace = Workspace::new("project".into());
        let pane = workspace.focused;
        let terminal = workspace.active();
        let browser = PaneSurface::browser("Source", "about:blank".into());
        let source = browser.id;
        workspace.root.add_surface_to_leaf(pane, browser).unwrap();
        (workspace, pane, source, terminal)
    }

    #[test]
    fn admission_bounds_live_or_reserved_popups_without_overflow() {
        for count in [0, MAX_POPUP_TABS - 1] {
            assert!(admit(count).is_ok());
        }
        for count in [MAX_POPUP_TABS, MAX_POPUP_TABS + 1, usize::MAX] {
            assert!(admit(count).is_err());
        }
    }

    #[test]
    fn popup_uses_source_pane_and_preserves_existing_tabs_and_other_panes() {
        let (mut workspace, source_pane, source, terminal) = browser_workspace();
        let sibling = workspace.split(SplitDirection::Vertical);
        let sibling_terminal = workspace.active();
        workspace
            .root
            .add_surface_to_leaf(
                sibling,
                PaneSurface::browser("Other browser", "https://example.com/other".into()),
            )
            .unwrap();
        assert_eq!(workspace.focused, sibling);
        let before = serde_json::to_value(&workspace).unwrap();
        let mut candidate = workspace.clone();
        let opened = open(&mut candidate, source, "https://example.com/popup".into()).unwrap();

        // Preparing a native tab must not commit to the live workspace early.
        assert_eq!(serde_json::to_value(&workspace).unwrap(), before);
        assert_eq!(opened.pane, source_pane);
        assert_ne!(opened.surface, source);
        assert_eq!(candidate.focused, source_pane);
        assert_eq!(candidate.active(), opened.surface);
        assert_eq!(candidate.leaves().len(), workspace.leaves().len());
        let Some(PaneContent::Tabs { active, surfaces }) =
            candidate.root.find_leaf_content(source_pane)
        else {
            panic!("source pane must keep its tabs");
        };
        assert_eq!(active, opened.surface);
        assert_eq!(
            surfaces.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            vec![terminal, source, opened.surface]
        );
        for surface in [terminal, source] {
            assert_eq!(
                serde_json::to_value(candidate.root.find_surface(source_pane, surface)).unwrap(),
                serde_json::to_value(workspace.root.find_surface(source_pane, surface)).unwrap()
            );
        }
        assert_eq!(
            serde_json::to_value(candidate.root.find_leaf_content(sibling)).unwrap(),
            serde_json::to_value(workspace.root.find_leaf_content(sibling)).unwrap()
        );
        assert!(candidate
            .root
            .find_surface(sibling, sibling_terminal)
            .is_some());
    }

    #[test]
    fn stale_inactive_and_terminal_sources_fail_without_changing_the_workspace() {
        let (workspace, pane, source, terminal) = browser_workspace();
        for rejected in [SurfaceId::new(), terminal, source] {
            let mut candidate = workspace.clone();
            if rejected == source {
                candidate.root.set_active_surface(pane, terminal);
            }
            let before = serde_json::to_value(&candidate).unwrap();
            assert!(open(&mut candidate, rejected, "about:blank".into()).is_err());
            assert_eq!(serde_json::to_value(&candidate).unwrap(), before);
        }
    }

    #[test]
    fn rejected_urls_leave_source_selection_layout_and_identity_untouched() {
        let (workspace, _, source, _) = browser_workspace();
        let before = serde_json::to_value(&workspace).unwrap();
        for url in [
            "javascript:alert(1)".to_owned(),
            "file:///C:/private".into(),
            "data:text/html,popup".into(),
            "flowmux-terminal://localhost".into(),
            "https://FLOWMUX-TERMINAL.localhost/".into(),
            "relative-path".into(),
            "https://example.com/\0".into(),
            format!("https://example.com/{}", "한".repeat(3000)),
        ] {
            let mut candidate = workspace.clone();
            assert!(open(&mut candidate, source, url).is_err());
            assert_eq!(serde_json::to_value(candidate).unwrap(), before);
        }
    }

    #[test]
    fn allowed_url_metadata_retains_encoded_unicode_without_normalization() {
        let (workspace, pane, source, _) = browser_workspace();
        let encoded =
            "https://example.com/%ED%95%9C%20%EA%B8%80?q=%E1%84%92%E1%85%A1%E1%86%AB#%F0%9F%98%80";
        for (input, expected) in [
            ("about:blank", "about:blank"),
            ("http://example.com/", "http://example.com/"),
            ("https://example.com/한 글?q=한#😀", encoded),
            (encoded, encoded),
        ] {
            let mut candidate = workspace.clone();
            let opened = open(&mut candidate, source, input.into()).unwrap();
            assert_eq!(opened.url, expected);
            let tab = candidate.root.find_surface(pane, opened.surface).unwrap();
            let SurfaceKind::Browser { initial_url } = tab.kind else {
                panic!("popup must create a browser tab");
            };
            assert_eq!(initial_url.as_deref(), Some(expected));
        }
    }

    #[test]
    fn closing_inactive_popup_preserves_active_tabs_and_focused_pane() {
        let (mut workspace, pane, source, terminal) = browser_workspace();
        let child = open(&mut workspace, source, "about:blank".into()).unwrap();
        assert!(workspace.root.set_active_surface(pane, source));
        let sibling = workspace.split(SplitDirection::Vertical);
        let sibling_before =
            serde_json::to_value(workspace.root.find_leaf_content(sibling)).unwrap();

        close(&mut workspace, child.surface).unwrap();

        assert_eq!(workspace.focused, sibling);
        assert_eq!(workspace.root.active_surface_id(pane), Some(source));
        assert_eq!(workspace.root.surface_count(pane), Some(2));
        assert!(workspace.root.find_surface(pane, terminal).is_some());
        assert!(workspace.root.find_surface(pane, child.surface).is_none());
        assert_eq!(
            serde_json::to_value(workspace.root.find_leaf_content(sibling)).unwrap(),
            sibling_before
        );
    }

    #[test]
    fn closing_only_popup_tab_collapses_its_pane_and_repairs_focus_only_if_needed() {
        let mut workspace = Workspace::new("project".into());
        let original_pane = workspace.focused;
        let original_root = serde_json::to_value(&workspace.root).unwrap();
        let child_pane = workspace.split(SplitDirection::Vertical);
        let temporary_terminal = workspace.active();
        let child = PaneSurface::browser("Popup", "about:blank".into());
        let child_id = child.id;
        workspace
            .root
            .add_surface_to_leaf(child_pane, child)
            .unwrap();
        workspace
            .root
            .close_surface_in_leaf(child_pane, temporary_terminal);

        for focused in [original_pane, child_pane] {
            let mut candidate = workspace.clone();
            candidate.focused = focused;
            close(&mut candidate, child_id).unwrap();
            assert_eq!(candidate.focused, original_pane);
            assert_eq!(serde_json::to_value(candidate.root).unwrap(), original_root);
        }
    }

    #[test]
    fn closing_missing_terminal_or_final_browser_tab_rejects_without_mutation() {
        let (workspace, pane, source, terminal) = browser_workspace();
        for rejected in [SurfaceId::new(), terminal, source] {
            let mut candidate = workspace.clone();
            if rejected == source {
                candidate.root.close_surface_in_leaf(pane, terminal);
            }
            let before = serde_json::to_value(&candidate).unwrap();
            assert!(close(&mut candidate, rejected).is_err());
            assert_eq!(serde_json::to_value(candidate).unwrap(), before);
        }
    }
}
