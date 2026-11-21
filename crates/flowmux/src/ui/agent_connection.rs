// SPDX-License-Identifier: GPL-3.0-or-later
//! A managed tab stays visibly pending until its relay acknowledges the snapshot.

use gtk::prelude::*;
use std::cell::Cell;
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
pub(crate) struct AgentViewTakeover {
    pub previous: Option<flowmux_ipc::agent_host::AttachmentKey>,
    pub token: uuid::Uuid,
    pub pane: flowmux_core::PaneId,
    pub workspace: flowmux_core::WorkspaceId,
}

pub(crate) struct AgentConnection {
    pub binding: flowmux_core::AgentViewBinding,
    pub started: Instant,
    pub was_ready: Cell<bool>,
    pub last_ready: Cell<Option<flowmux_ipc::agent_host::AttachmentKey>>,
    pub takeover: Option<AgentViewTakeover>,
    /// The agent's project directory. The tab's own process is only the relay.
    pub project: Option<std::path::PathBuf>,
    /// Native session last saved for this tab, so a start without a Host can resume it.
    pub recorded_session: std::cell::RefCell<Option<String>>,
    /// Removal of that saved session once the execution has ended: `Some(true)` when done.
    pub session_cleared: std::rc::Rc<Cell<Option<bool>>>,
    /// An ordinary terminal's shell: it types and reads like any terminal, with no
    /// connection notice, while its execution lives in the Host.
    pub shell: bool,
    /// The shell's process in the Host; this tab's own child is only the relay.
    pub host_pid: Cell<Option<u32>>,
    root: gtk::Box,
    spinner: gtk::Spinner,
    label: gtk::Label,
}

impl AgentConnection {
    pub fn new(binding: flowmux_core::AgentViewBinding, overlay: &gtk::Overlay) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Start);
        root.set_margin_top(12);
        root.add_css_class("card");
        let spinner = gtk::Spinner::new();
        let label = gtk::Label::new(Some("Connecting to Agent…"));
        label.set_wrap(true);
        root.append(&spinner);
        root.append(&label);
        overlay.add_overlay(&root);
        spinner.start();
        Self {
            binding,
            started: Instant::now(),
            was_ready: Cell::new(false),
            last_ready: Cell::new(None),
            takeover: None,
            project: None,
            recorded_session: Default::default(),
            session_cleared: Default::default(),
            shell: false,
            host_pid: Cell::new(None),
            root,
            spinner,
            label,
        }
    }

    pub fn render(&self, ready: bool, error: Option<&str>) {
        self.root.set_visible(!ready);
        if ready {
            self.was_ready.set(true);
        }
        self.spinner.set_visible(!ready && error.is_none());
        self.spinner.set_spinning(!ready && error.is_none());
        self.label.set_text(error.unwrap_or("Connecting to Agent…"));
    }

    /// `elsewhere`: the owner's relay is not this tab's. A terminal taken by another window
    /// keeps its tab id there, so the id alone cannot tell the two tabs apart.
    pub fn superseded_by(
        &self,
        surface: flowmux_core::SurfaceId,
        owner: flowmux_ipc::agent_host::AgentViewOwner,
        elsewhere: bool,
    ) -> bool {
        self.binding.instance == owner.key.instance
            && self.binding.epoch == owner.key.epoch
            && (surface != owner.surface || elsewhere)
            && match self.last_ready.get() {
                Some(previous) => {
                    previous.instance == owner.key.instance
                        && previous.epoch == owner.key.epoch
                        && previous.generation < owner.key.generation
                }
                // An explicit open may be preparing to replace this owner.
                None => self.takeover.is_none(),
            }
    }
}

/// Whether the execution's ready view belongs to another terminal than the one running
/// `terminal_pid`: its relay is neither that process nor its child.
pub(crate) fn shown_elsewhere(
    terminal_pid: Option<u32>,
    agent: &flowmux_ipc::agent_host::AgentInstance,
) -> bool {
    agent.attachment.as_ref().is_some_and(|attachment| {
        attachment.ready
            && terminal_pid != attachment.relay_pid
            && attachment
                .relay_pid
                .and_then(flowmux_procmon::parent_pid)
                .is_some_and(|parent| Some(parent) != terminal_pid)
    })
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[gtk::test]
    fn connection_shows_pending_error_and_verified_ready() {
        let overlay = gtk::Overlay::new();
        let connection = AgentConnection::new(
            flowmux_core::AgentViewBinding {
                instance: flowmux_core::AgentInstanceId::new(),
                epoch: uuid::Uuid::new_v4(),
            },
            &overlay,
        );
        connection.render(false, None);
        assert!(connection.root.is_visible() && connection.spinner.is_spinning());
        assert!(!connection.was_ready.get());
        connection.render(false, Some("Connection failed"));
        assert!(connection.root.is_visible() && !connection.spinner.is_spinning());
        assert_eq!(connection.label.text(), "Connection failed");
        connection.render(true, None);
        assert!(!connection.root.is_visible() && connection.was_ready.get());
        connection.render(false, Some("Host unavailable"));
        assert!(connection.root.is_visible() && connection.was_ready.get());
    }

    #[gtk::test]
    fn committed_surface_reconciles_restores_and_preserves_explicit_pending_opens() {
        use flowmux_ipc::agent_host::{AgentViewOwner, AttachmentKey};
        let binding = flowmux_core::AgentViewBinding {
            instance: flowmux_core::AgentInstanceId::new(),
            epoch: uuid::Uuid::new_v4(),
        };
        let mut connection = AgentConnection::new(binding, &gtk::Overlay::new());
        let surface = flowmux_core::SurfaceId::new();
        let previous = AttachmentKey {
            instance: binding.instance,
            epoch: binding.epoch,
            lease: uuid::Uuid::new_v4(),
            generation: 2,
        };
        let owner = AgentViewOwner {
            key: AttachmentKey {
                generation: 3,
                lease: uuid::Uuid::new_v4(),
                ..previous
            },
            surface: flowmux_core::SurfaceId::new(),
        };
        // Restores have no observed lease, including after the winner detaches.
        assert!(connection.superseded_by(surface, owner, false));
        assert!(!connection.superseded_by(owner.surface, owner, false));
        // The same tab id in another window is another tab.
        assert!(connection.superseded_by(owner.surface, owner, true));
        connection.takeover = Some(AgentViewTakeover {
            previous: Some(previous),
            token: uuid::Uuid::new_v4(),
            pane: flowmux_core::PaneId::new(),
            workspace: flowmux_core::WorkspaceId::new(),
        });
        assert!(!connection.superseded_by(surface, owner, false));
        connection.last_ready.set(Some(previous));
        assert!(connection.superseded_by(surface, owner, false));
        for key in [
            AttachmentKey {
                generation: 2,
                ..owner.key
            },
            AttachmentKey {
                generation: 1,
                ..owner.key
            },
            AttachmentKey {
                epoch: uuid::Uuid::new_v4(),
                ..owner.key
            },
            AttachmentKey {
                instance: flowmux_core::AgentInstanceId::new(),
                ..owner.key
            },
        ] {
            assert!(!connection.superseded_by(surface, AgentViewOwner { key, ..owner }, false));
        }
    }
}
