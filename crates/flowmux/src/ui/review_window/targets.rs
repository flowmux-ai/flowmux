// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::bridge::{Bridge, GtkCommand};
use flowmux_core::{AgentPresence, SurfaceId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReviewTarget {
    pub surface: SurfaceId,
    pub name: String,
    pub pid: Option<u32>,
    pub session: Option<String>,
    pub label: String,
}

impl ReviewTarget {
    pub fn matches(&self, presence: &AgentPresence) -> bool {
        self.name == presence.name
            && self.pid == presence.pid
            && self.session == presence.session_id
    }
}

pub(super) struct Targets {
    pub row: gtk::Box,
    chooser: gtk::DropDown,
    labels: gtk::StringList,
    refresh: gtk::Button,
    pub focus: gtk::Button,
    items: RefCell<Vec<ReviewTarget>>,
    bridge: RefCell<Option<Bridge>>,
}

impl Targets {
    pub fn new() -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let labels = gtk::StringList::new(&["No live agents — Copy review is available"]);
        let chooser = gtk::DropDown::new(Some(labels.clone()), None::<gtk::Expression>);
        chooser.set_hexpand(true);
        chooser.update_property(&[gtk::accessible::Property::Label("Review recipient")]);
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            item.set_child(Some(
                &gtk::Label::builder()
                    .xalign(0.0)
                    .ellipsize(pango::EllipsizeMode::End)
                    .max_width_chars(40)
                    .build(),
            ));
        });
        factory.connect_bind(|_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let value = item
                .item()
                .and_downcast::<gtk::StringObject>()
                .unwrap()
                .string();
            let label = item.child().and_downcast::<gtk::Label>().unwrap();
            label.set_text(&value);
            label.set_tooltip_text(Some(&value));
        });
        chooser.set_factory(Some(&factory));
        let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh.set_tooltip_text(Some("Refresh live agents"));
        let focus = gtk::Button::with_label("Copy & focus agent");
        focus.set_sensitive(false);
        focus.set_tooltip_text(Some("Validate and copy the review, then focus the selected agent. Paste and submit when ready; existing input is preserved."));
        row.append(&chooser);
        row.append(&refresh);
        Self {
            row,
            chooser,
            labels,
            refresh,
            focus,
            items: RefCell::new(Vec::new()),
            bridge: RefCell::new(None),
        }
    }

    pub fn selected(&self) -> Option<ReviewTarget> {
        self.items
            .borrow()
            .get(self.chooser.selected().checked_sub(1)? as usize)
            .cloned()
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.focus
            .set_sensitive(enabled && self.selected().is_some() && self.bridge.borrow().is_some());
    }

    pub fn send(&self, command: GtkCommand) -> bool {
        self.bridge
            .borrow()
            .as_ref()
            .is_some_and(|b| b.tx.try_send(command).is_ok())
    }
}

impl ReviewWindow {
    pub fn connect_targets(self: &Rc<Self>, bridge: Bridge) {
        *self.targets.bridge.borrow_mut() = Some(bridge);
        let weak = Rc::downgrade(self);
        self.targets.refresh.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.targets.send(GtkCommand::RefreshReviewTargets);
            }
        });
        let weak = Rc::downgrade(self);
        self.targets.chooser.connect_selected_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.update_delivery_controls();
            }
        });
        let weak = Rc::downgrade(self);
        self.targets.focus.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.deliver_to_agent();
            }
        });
    }

    pub fn set_targets(&self, targets: Vec<ReviewTarget>) {
        if *self.targets.items.borrow() == targets {
            return;
        }
        let previous = self.targets.selected();
        let selected = previous
            .and_then(|p| {
                targets.iter().position(|t| {
                    t.surface == p.surface
                        && t.name == p.name
                        && t.pid == p.pid
                        && t.session == p.session
                })
            })
            .map_or(0, |i| i as u32 + 1);
        let mut labels = vec![if targets.is_empty() {
            "No live agents — Copy review is available"
        } else {
            "Select a live agent"
        }];
        labels.extend(targets.iter().map(|t| t.label.as_str()));
        self.targets
            .labels
            .splice(0, self.targets.labels.n_items(), &labels);
        *self.targets.items.borrow_mut() = targets;
        self.targets.chooser.set_selected(selected);
        self.update_delivery_controls();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_core::{AgentActivity, AgentStatus};
    #[test]
    fn target_identity_is_provider_and_activity_independent() {
        for name in [
            "claude",
            "codex",
            "gemini",
            "cline",
            "opencode",
            "agy",
            "custom-agent",
        ] {
            let mut presence = AgentPresence::new(name, AgentActivity::Idle, Some(42));
            presence.session_id = Some("session-a".into());
            let target = ReviewTarget {
                surface: SurfaceId::new(),
                name: name.into(),
                pid: Some(42),
                session: Some("session-a".into()),
                label: name.into(),
            };
            for status in [
                AgentStatus::Unknown,
                AgentStatus::Idle,
                AgentStatus::Working,
                AgentStatus::Blocked,
                AgentStatus::Done,
            ] {
                presence.status = status;
                assert!(target.matches(&presence));
            }
            presence.pid = Some(43);
            assert!(!target.matches(&presence));
            presence.pid = Some(42);
            presence.session_id = Some("session-b".into());
            assert!(!target.matches(&presence));
            presence.session_id = Some("session-a".into());
            presence.name = "replaced".into();
            assert!(!target.matches(&presence));
        }
    }
}

#[cfg(test)]
pub(super) async fn smoke(review: &Rc<ReviewWindow>) {
    let (bridge, receiver) = Bridge::new();
    review.connect_targets(bridge);
    assert!(!review.targets.focus.is_sensitive());
    let target = ReviewTarget {
        surface: SurfaceId::new(),
        name: "custom-agent".into(),
        pid: None,
        session: None,
        label: "custom-agent · fixture / terminal · blocked".into(),
    };
    review.set_targets(vec![target.clone()]);
    review.targets.chooser.set_selected(1);
    assert!(review.targets.focus.is_sensitive());
    review.targets.focus.emit_clicked();
    let command = glib::future_with_timeout(std::time::Duration::from_secs(20), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    match command {
        GtkCommand::FocusReviewTarget {
            target: received,
            prompt,
            ..
        } => {
            assert_eq!(received, target);
            assert!(prompt.contains("Code review feedback"));
            assert!(prompt.contains("preserve conflict draft"));
        }
        other => panic!("unexpected handoff command: {other:?}"),
    }
    review.set_targets(Vec::new());
    assert!(!review.targets.focus.is_sensitive());
    println!("DIFF_REVIEW_HANDOFF_BUTTON_OK");
}
