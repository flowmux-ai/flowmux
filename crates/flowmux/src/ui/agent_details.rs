// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only, on-demand explanations shared by both live agent lists.

use crate::bridge::{Bridge, GtkCommand};
use flowmux_core::{AgentAttention, AgentWaitSummary, SurfaceId};
use flowmux_ipc::protocol::AgentExplanation;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

pub(super) fn row(button: &gtk::Button, surface: SurfaceId, bridge: Bridge) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    button.set_hexpand(true);
    root.append(button);
    let details = AgentDetails::new(surface, bridge);
    root.append(&details.button);
    root.connect_destroy(move |_| {
        let _ = &details;
    });
    root
}

struct AgentDetails {
    button: gtk::MenuButton,
    popover: gtk::Popover,
    summary: gtk::Label,
    children: gtk::Label,
    open: gtk::Button,
    snapshot: RefCell<Option<AgentExplanation>>,
    pending: Cell<bool>,
    timer: RefCell<Option<glib::SourceId>>,
    placement: Cell<u64>,
    surface: SurfaceId,
    bridge: Bridge,
}

impl AgentDetails {
    fn new(surface: SurfaceId, bridge: Bridge) -> Rc<Self> {
        let button = gtk::MenuButton::builder()
            .icon_name("dialog-information-symbolic")
            .tooltip_text("Agent details")
            .valign(gtk::Align::Center)
            .build();
        button.add_css_class("flat");
        button.set_widget_name(&format!("flowmux-agent-details-{surface}"));
        button.update_property(&[gtk::accessible::Property::Label("Agent details")]);
        let popover = gtk::Popover::new();
        popover.set_position(gtk::PositionType::Right);
        super::popover_pos::set_menu_popover(&button, &popover);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        let heading = gtk::Label::new(Some("Agent details"));
        heading.set_xalign(0.0);
        heading.add_css_class("heading");
        content.append(&heading);
        let summary = detail_label("Loading current state…");
        let children = detail_label("No child observation data");
        let expander = gtk::Expander::builder()
            .label("Observed child work")
            .child(&children)
            .build();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.append(&summary);
        body.append(&expander);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .max_content_height(400)
            .min_content_width(340)
            .child(&body)
            .build();
        content.append(&scroll);
        let open = gtk::Button::with_label("Open terminal");
        open.set_sensitive(false);
        content.append(&open);
        popover.set_child(Some(&content));
        let this = Rc::new(Self {
            button: button.clone(),
            popover: popover.clone(),
            summary,
            children,
            open,
            snapshot: RefCell::new(None),
            pending: Cell::new(false),
            timer: RefCell::new(None),
            placement: Cell::new(0),
            surface,
            bridge,
        });
        // The row owns the view; callbacks and asynchronous reads use weak
        // references so removing a live row also releases the popup and timer.
        let weak = Rc::downgrade(&this);
        popover.connect_show(move |_| {
            if let Some(this) = weak.upgrade() {
                this.position();
                this.refresh();
                let weak = Rc::downgrade(&this);
                *this.timer.borrow_mut() =
                    Some(glib::timeout_add_local(Duration::from_secs(1), move || {
                        let Some(this) = weak.upgrade() else {
                            return glib::ControlFlow::Break;
                        };
                        this.refresh();
                        glib::ControlFlow::Continue
                    }));
            }
        });
        let weak = Rc::downgrade(&this);
        expander.connect_expanded_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.position();
            }
        });
        let weak = Rc::downgrade(&this);
        popover.connect_hide(move |_| {
            if let Some(this) = weak.upgrade() {
                if let Some(timer) = this.timer.borrow_mut().take() {
                    timer.remove();
                }
            }
        });
        let weak = Rc::downgrade(&this);
        this.open.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                if let Some(agent) = this.snapshot.borrow().as_ref() {
                    let _ = this.bridge.tx.try_send(GtkCommand::OpenAgentTerminal {
                        surface: agent.surface,
                        session_key: agent.session_key,
                    });
                }
                this.button.popdown();
            }
        });
        this
    }

    fn refresh(self: &Rc<Self>) {
        if self.pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        let bridge = self.bridge.clone();
        let surface = self.surface;
        glib::MainContext::default().spawn_local(async move {
            let (ack, response) = tokio::sync::oneshot::channel();
            let query = async {
                bridge
                    .send(GtkCommand::QueryAgentExplanation { surface, ack })
                    .await
                    .ok()?;
                response.await.ok()
            };
            let result = glib::future_with_timeout(Duration::from_secs(3), query).await;
            if let Some(this) = weak.upgrade() {
                this.pending.set(false);
                if !this.popover.is_visible() {
                    return;
                }
                match result {
                    Ok(Some(Some(agent))) => this.render(agent),
                    other => {
                        this.snapshot.borrow_mut().take();
                        this.open.set_sensitive(false);
                        this.summary.set_label(if matches!(other, Ok(Some(None))) {
                            "This agent is no longer available."
                        } else {
                            "Current state is unavailable. Retrying…"
                        });
                        this.children.set_label("No child observation data");
                    }
                }
            }
        });
    }

    fn position(self: &Rc<Self>) {
        super::popover_pos::anchor_at_click(
            &self.popover,
            &self.button,
            0.0,
            self.button.height() as f64,
        );
        // Wrapped labels can grow after GTK's initial measurement. Clamp the
        // allocated popup surface too, after the layout frame has completed.
        let generation = self.placement.get().wrapping_add(1);
        self.placement.set(generation);
        let weak = Rc::downgrade(self);
        let frames = Cell::new(0);
        self.popover.add_tick_callback(move |popover, _| {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if this.placement.get() != generation || !popover.is_visible() {
                return glib::ControlFlow::Break;
            }
            frames.set(frames.get() + 1);
            if frames.get() == 1 {
                return glib::ControlFlow::Continue;
            }
            let Some(popup) = popover.surface().and_downcast::<gtk::gdk::Popup>() else {
                return glib::ControlFlow::Break;
            };
            let Some(parent) = gtk::gdk::prelude::PopupExt::parent(&popup) else {
                return glib::ControlFlow::Break;
            };
            let dx = popup.position_x().min(0)
                + (popup.position_x() + popup.width() - parent.width()).max(0);
            let dy = popup.position_y().min(0)
                + (popup.position_y() + popup.height() - parent.height()).max(0);
            if dx == 0 && dy == 0 {
                return glib::ControlFlow::Break;
            }
            let (_, mut rect) = popover.pointing_to();
            rect.set_x(rect.x() - dx);
            rect.set_y(rect.y() - dy);
            popover.set_pointing_to(Some(&rect));
            if frames.get() < 4 {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
    }

    fn render(self: &Rc<Self>, agent: AgentExplanation) {
        if self.snapshot.borrow().as_ref() == Some(&agent) {
            return;
        }
        let status = match agent.attention {
            AgentAttention::InputRequired => "Input required",
            AgentAttention::FailureReview => "Failure to review",
            AgentAttention::ResultReview => "Result to review",
            AgentAttention::Waiting => "Waiting",
            AgentAttention::Working => "Working",
            AgentAttention::Unconfirmed => "Activity unconfirmed",
            AgentAttention::Idle => "Idle",
        };
        let mut text = flowmux_core::agent_status_tooltip(
            &agent.agent,
            status,
            agent.status_evidence,
            &agent.pending_waits,
        );
        let identity = match agent.identity_source.as_deref() {
            Some("flowmux:hook") => "Native hook",
            Some("flowmux:screen") => "Terminal screen",
            Some("flowmux:proc") => "Process detection",
            _ => "Unconfirmed",
        };
        text.push_str(&format!("\nIdentity: {identity}"));
        if let Some(outcome) = agent.turn_outcome {
            text.push_str(&format!("\n{}", outcome.status_text()));
        }
        if agent.pending_waits.is_empty() {
            text.push_str("\nNo unresolved requests observed");
        }
        text.push_str("\nReading details does not resolve requests.");
        self.summary.set_label(&text);
        self.children.set_label(&child_details(&agent));
        self.open.set_label(if agent.children.is_some() {
            "Open parent terminal"
        } else {
            "Open terminal"
        });
        self.open.set_sensitive(true);
        *self.snapshot.borrow_mut() = Some(agent);
        self.position();
    }
}

fn detail_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(48);
    label.set_selectable(true);
    label
}

fn child_details(agent: &AgentExplanation) -> String {
    let Some(children) = &agent.children else {
        return "No child observation data. This does not establish that there are no child tasks."
            .into();
    };
    let mut text = if children.active.is_empty() {
        "No active children observed".into()
    } else {
        format!("{} active children observed", children.active.len())
    };
    if children.parent_completion_pending {
        text.push_str("\nParent response ended; observed child work remains.");
    }
    for child in &children.active {
        text.push_str(&format!("\n\nChild: {}\nTurn: {}", child.id, child.turn_id));
        if child.pending_waits.is_empty() {
            text.push_str("\nNo unresolved requests observed");
        }
        for wait in &child.pending_waits {
            append_wait(&mut text, &wait.wait);
        }
    }
    if !children.unattributed_waits.is_empty() {
        text.push_str("\n\nChild requests without a matching active observation:");
        for wait in &children.unattributed_waits {
            text.push_str(&format!("\nScope: {}", wait.scope_id));
            append_wait(&mut text, &wait.wait);
        }
    }
    text.push_str("\n\nPartial hook observations, not a complete task inventory. Requests are handled in the parent terminal.");
    text
}

fn append_wait(text: &mut String, wait: &AgentWaitSummary) {
    use flowmux_core::{AgentRetryState, AgentWaitActor, AgentWaitScope};
    let scope = match wait.scope {
        AgentWaitScope::Tool => "Tool requests",
        AgentWaitScope::Permission => "Permission scopes",
        AgentWaitScope::Session => "Session waits",
    };
    let actor = match wait.actor {
        AgentWaitActor::User => "user",
        AgentWaitActor::Agent => "agent",
        AgentWaitActor::External => "external service",
        AgentWaitActor::Unknown => "unconfirmed",
    };
    text.push_str(&format!(
        "\n{scope}: {} · {} · waiting on {actor}\nSince {}",
        wait.count,
        wait.status_text().unwrap_or("Reason unavailable"),
        wait.since.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    match wait.retry {
        AgentRetryState::Automatic => text.push_str(" · automatic retry"),
        AgentRetryState::Manual => text.push_str(" · retry requires input"),
        AgentRetryState::Unknown => {}
    }
    if wait
        .context_evidence
        .is_some_and(|e| e.source == flowmux_core::AgentEvidenceSource::Screen)
    {
        text.push_str(" · retry inferred from screen");
    }
}

#[cfg(test)]
pub(crate) async fn check_agent_details() {
    use flowmux_core::*;
    use flowmux_ipc::protocol::{AgentChildExplanation, AgentChildWait, AgentChildrenExplanation};
    let (bridge, rx) = Bridge::new();
    let surface = SurfaceId::new();
    let root = row(&gtk::Button::with_label("Agent"), surface, bridge);
    root.set_valign(gtk::Align::End);
    let button = root
        .last_child()
        .unwrap()
        .downcast::<gtk::MenuButton>()
        .unwrap();
    let window = gtk::Window::builder()
        .default_width(700)
        .default_height(600)
        .child(&root)
        .build();
    window.present();
    glib::timeout_future(Duration::from_millis(50)).await;
    assert!(rx.try_recv().is_err(), "closed details must not query");
    let wait = AgentWaitSummary {
        scope: AgentWaitScope::Permission,
        reason: AgentWaitReason::Approval,
        actor: AgentWaitActor::User,
        retry: AgentRetryState::Unknown,
        context_evidence: None,
        count: 1,
        since: chrono::Utc::now(),
    };
    let mut agent = AgentExplanation {
        workspace: WorkspaceId::new(),
        pane: PaneId::new(),
        surface,
        agent: "test agent".into(),
        status: AgentStatus::Blocked,
        attention: AgentAttention::InputRequired,
        seen: false,
        identity_source: Some("flowmux:hook".into()),
        status_evidence: Some(AgentStatusEvidence {
            source: AgentEvidenceSource::Hook,
            since: wait.since,
        }),
        turn_outcome: None,
        sequence: Some(4),
        session_key: Some(12),
        pending_waits: vec![wait],
        children: None,
    };
    assert!(child_details(&agent).contains("does not establish"));
    agent.children = Some(AgentChildrenExplanation {
        active: vec![AgentChildExplanation {
            id: "<child>".into(),
            turn_id: "turn-one".into(),
            pending_waits: vec![AgentChildWait {
                scope_id: "matched".into(),
                wait,
            }],
        }],
        parent_completion_pending: true,
        unattributed_waits: vec![AgentChildWait {
            scope_id: "unmatched".into(),
            wait,
        }],
    });
    button.popup();
    let command = glib::future_with_timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let GtkCommand::QueryAgentExplanation {
        surface: queried,
        ack,
    } = command
    else {
        panic!("opening details must only query");
    };
    assert_eq!(queried, surface);
    ack.send(Some(agent.clone())).unwrap();
    glib::timeout_future(Duration::from_millis(50)).await;
    let popover = button.popover().unwrap();
    let content = popover.child().unwrap().downcast::<gtk::Box>().unwrap();
    let scroll = content
        .first_child()
        .unwrap()
        .next_sibling()
        .unwrap()
        .downcast::<gtk::ScrolledWindow>()
        .unwrap();
    let body = scroll.child().unwrap().first_child().unwrap();
    let summary = body
        .first_child()
        .unwrap()
        .downcast::<gtk::Label>()
        .unwrap();
    let expander = body
        .last_child()
        .unwrap()
        .downcast::<gtk::Expander>()
        .unwrap();
    let children = expander.child().unwrap().downcast::<gtk::Label>().unwrap();
    let open = content
        .last_child()
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    assert!(summary.text().contains("Hook-reported state"));
    assert!(summary.text().contains("waiting on user"));
    assert!(children.text().contains("<child>"));
    assert!(children
        .text()
        .contains("without a matching active observation"));
    assert!(children.text().contains("Parent response ended"));
    assert!(!children.uses_markup());
    assert_eq!(open.label().as_deref(), Some("Open parent terminal"));
    expander.set_expanded(true);
    glib::timeout_future(Duration::from_millis(50)).await;
    let popup = popover
        .surface()
        .unwrap()
        .downcast::<gtk::gdk::Popup>()
        .unwrap();
    glib::future_with_timeout(Duration::from_secs(2), async {
        while popup.position_y() < 0 || popup.position_y() + popup.height() > window.height() {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("allocated details must fit inside the window");
    assert!(
        popup.position_y() >= 0 && popup.position_y() + popup.height() <= window.height(),
        "details must fit: y={} height={} window={}",
        popup.position_y(),
        popup.height(),
        window.height()
    );
    drop(popup);
    open.grab_focus();
    let old_summary = summary.clone();
    agent.pending_waits.clear();
    agent.status = AgentStatus::Done;
    agent.attention = AgentAttention::ResultReview;
    agent.turn_outcome = Some(AgentTurnOutcome::Unknown);
    agent.children = Some(AgentChildrenExplanation {
        active: vec![],
        parent_completion_pending: false,
        unattributed_waits: vec![],
    });
    let command = glib::future_with_timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let GtkCommand::QueryAgentExplanation { ack, .. } = command else {
        panic!("refresh must only query");
    };
    ack.send(Some(agent.clone())).unwrap();
    glib::timeout_future(Duration::from_millis(50)).await;
    assert_eq!(body.first_child(), Some(old_summary.upcast()));
    assert!(open.is_focus());
    assert!(expander.is_expanded());
    assert!(children.text().contains("No active children observed"));
    assert!(!children.text().contains("<child>"));
    assert!(summary.text().contains("outcome unconfirmed"));
    assert!(!summary.text().contains("waiting on user"));
    open.emit_clicked();
    assert!(
        matches!(rx.try_recv().unwrap(), GtkCommand::OpenAgentTerminal { surface: target, session_key: Some(12) } if target == surface)
    );
    assert!(!popover.is_visible());
    glib::timeout_future(Duration::from_millis(1100)).await;
    assert!(rx.try_recv().is_err(), "hidden details must stop polling");
    button.popup();
    let command = glib::future_with_timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let GtkCommand::QueryAgentExplanation { ack, .. } = command else {
        panic!("reopening must query");
    };
    ack.send(None).unwrap();
    glib::timeout_future(Duration::from_millis(50)).await;
    assert!(!open.is_sensitive());
    assert!(summary.text().contains("no longer available"));
    button.popdown();
    let weak = button.downgrade();
    drop(button);
    drop(popover);
    drop(content);
    drop(scroll);
    drop(body);
    drop(summary);
    drop(expander);
    drop(children);
    drop(open);
    window.set_child(None::<&gtk::Widget>);
    drop(root);
    glib::timeout_future(Duration::from_millis(50)).await;
    assert!(
        weak.upgrade().is_none(),
        "removed rows must release details and callbacks"
    );
    window.destroy();
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    #[gtk::test]
    async fn live_details_refresh_without_navigation_and_release_on_removal() {
        super::check_agent_details().await;
    }
}
