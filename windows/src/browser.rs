// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows browser routing and URL policy, separate from the trusted terminal.
use crate::model::Workspace;
use anyhow::Context;
use flowmux_core::{PaneContent, PaneId, PaneSurface, SplitDirection, SurfaceId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_URL_BYTES: usize = 16 * 1024;
pub fn url(value: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        value.len() <= MAX_URL_BYTES && !value.chars().any(char::is_control),
        "invalid or oversized browser URL"
    );
    if value == "about:blank" {
        return Ok(value.into());
    }
    let parsed = url::Url::parse(value).context("enter an absolute http or https URL")?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some(),
        "browser supports http, https and about:blank URLs"
    );
    anyhow::ensure!(
        parsed.host_str() != Some("flowmux-terminal.localhost"),
        "the terminal origin is reserved"
    );
    let result = parsed.to_string();
    anyhow::ensure!(
        result.len() <= MAX_URL_BYTES,
        "encoded browser URL exceeds 16 KiB"
    );
    Ok(result)
}

#[derive(Debug, Clone, clap::Subcommand, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// Reuse a browser pane to the right, or split beside the source.
    Open {
        url: String,
        #[arg(long, conflicts_with = "down")]
        #[serde(default)]
        right: bool,
        #[arg(long)]
        #[serde(default)]
        down: bool,
        #[arg(long, value_parser = crate::command::parse_id)]
        pane: Option<Uuid>,
    },
    Navigate {
        #[arg(value_parser = crate::command::parse_id)]
        pane: Uuid,
        url: String,
    },
    Back {
        #[arg(value_parser = crate::command::parse_id)]
        pane: Uuid,
    },
    Forward {
        #[arg(value_parser = crate::command::parse_id)]
        pane: Uuid,
    },
    Reload {
        #[arg(value_parser = crate::command::parse_id)]
        pane: Uuid,
    },
    Stop {
        #[arg(value_parser = crate::command::parse_id)]
        pane: Uuid,
    },
    Url {
        #[arg(value_parser = crate::command::parse_id)]
        pane: Uuid,
    },
    Title {
        #[arg(value_parser = crate::command::parse_id)]
        pane: Uuid,
    },
    Status {
        #[arg(value_parser = crate::command::parse_id)]
        pane: Uuid,
    },
    Zoom {
        #[arg(value_parser = crate::command::parse_id)]
        pane: Uuid,
        scale: f64,
    },
    /// Evaluate synchronous JavaScript in the active browser document.
    Eval {
        #[arg(value_parser = crate::command::parse_id)]
        pane: Uuid,
        source: String,
    },
}

pub struct Opened {
    pub pane: PaneId,
    pub surface: SurfaceId,
    pub placement: &'static str,
}
/// Update a candidate workspace. Native view creation can fail without changing
/// the live layout; callers install this candidate only after the view exists.
pub fn open(
    workspace: &mut Workspace,
    source: PaneId,
    url: String,
    down: bool,
) -> anyhow::Result<Opened> {
    anyhow::ensure!(
        workspace.root.find_leaf_content(source).is_some(),
        "source pane not found"
    );
    let surface = PaneSurface::browser("Browser", url);
    let id = surface.id;
    let reuse = (!down)
        .then(|| workspace.root.find_right_sibling_browser_leaf(source))
        .flatten();
    let (pane, placement) = if let Some(pane) = reuse {
        workspace
            .root
            .add_surface_to_leaf(pane, surface)
            .context("browser destination disappeared")?;
        (pane, "reuse_right_sibling")
    } else {
        let direction = if down {
            SplitDirection::Horizontal
        } else {
            SplitDirection::Vertical
        };
        let pane = workspace
            .root
            .split_leaf(
                source,
                direction,
                0.5,
                PaneContent::Tabs {
                    active: id,
                    surfaces: vec![surface],
                },
            )
            .context("source pane disappeared")?;
        (pane, if down { "split_down" } else { "split_right" })
    };
    workspace.focused = pane;
    Ok(Opened {
        pane,
        surface: id,
        placement,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_policy_preserves_unicode_meaning_and_excludes_terminal_and_local_privilege() {
        assert_eq!(
            url("https://example.com/한 글?q=한#😀").unwrap(),
            "https://example.com/%ED%95%9C%20%EA%B8%80?q=%E1%84%92%E1%85%A1%E1%86%AB#%F0%9F%98%80"
        );
        assert_eq!(url("about:blank").unwrap(), "about:blank");
        for value in [
            "javascript:alert(1)",
            "file:///C:/data",
            "data:text/html,test",
            "flowmux-terminal://localhost",
            "http://FLOWMUX-TERMINAL.localhost/",
            "example.com",
            "https://example.com/\0",
        ] {
            assert!(url(value).is_err(), "{value}");
        }
        assert!(url(&format!("https://example.com/{}", "한".repeat(3000))).is_err());
    }
    #[test]
    fn browser_placement_reuses_right_sibling_and_preserves_source_terminal_identity() {
        let mut ws = Workspace::new("project".into());
        let source = ws.focused;
        let terminal = ws.active();
        let first = open(&mut ws, source, "about:blank".into(), false).unwrap();
        assert_eq!(first.placement, "split_right");
        let second = open(&mut ws, source, "https://example.com/".into(), false).unwrap();
        assert_eq!(second.placement, "reuse_right_sibling");
        assert_eq!(second.pane, first.pane);
        assert_ne!(second.surface, first.surface);
        assert_eq!(ws.root.active_surface_id(source), Some(terminal));
        let down = open(&mut ws, source, "about:blank".into(), true).unwrap();
        assert_eq!(down.placement, "split_down");
        assert_ne!(down.pane, first.pane);
        let before = serde_json::to_value(&ws).unwrap();
        assert!(open(&mut ws, PaneId::new(), "about:blank".into(), false).is_err());
        assert_eq!(serde_json::to_value(&ws).unwrap(), before);
    }
}
