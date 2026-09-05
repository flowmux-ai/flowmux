// SPDX-License-Identifier: GPL-3.0-or-later
//! Full-window workspace overview and its snapshot transition.

use super::*;

const THUMBNAIL_MAX_WIDTH: i32 = 320;
const THUMBNAIL_MAX_HEIGHT: i32 = 200;

#[derive(Clone, Default)]
pub(super) struct WorkspaceOverviewState {
    active: Rc<RefCell<Option<ActiveWorkspaceOverview>>>,
    transitioning: Rc<Cell<bool>>,
    generation: Rc<Cell<u64>>,
    tick: Rc<RefCell<Option<gtk::TickCallbackId>>>,
    dismiss_pending: Rc<Cell<bool>>,
}

struct ActiveWorkspaceOverview {
    root: gtk::Overlay,
    chrome: gtk::Box,
    transition_layer: gtk::Fixed,
    title: gtk::Label,
    flow: gtk::FlowBox,
    cards: Vec<WorkspaceOverviewCard>,
    active_workspace: Option<WorkspaceId>,
    saved_focus: Option<glib::WeakRef<gtk::Widget>>,
    saved_window_title: Option<String>,
    _native_views_suspend: crate::ui::browser_pane::NativeBrowserViewsSuspend,
}

#[derive(Clone)]
struct WorkspaceOverviewCard {
    workspace: WorkspaceId,
    root: gtk::Overlay,
    button: gtk::Button,
    picture: gtk::Picture,
    texture: Option<gtk::gdk::Texture>,
}

struct WorkspaceOverviewEntry {
    workspace: WorkspaceId,
    name: String,
    texture: Option<gtk::gdk::Texture>,
}

struct WorkspaceOverviewView {
    root: gtk::Overlay,
    chrome: gtk::Box,
    transition_layer: gtk::Fixed,
    title: gtk::Label,
    flow: gtk::FlowBox,
    cards: Vec<WorkspaceOverviewCard>,
}

impl WorkspaceOverviewState {
    pub(super) fn is_active(&self) -> bool {
        self.active.borrow().is_some()
    }

    fn next_generation(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        generation
    }
}

impl WindowController {
    pub(super) fn toggle_workspace_overview(&self) {
        if self.workspace_overview.transitioning.get() {
            self.workspace_overview.dismiss_pending.set(true);
            return;
        }
        if self.workspace_overview.is_active() {
            self.close_workspace_overview(None);
        } else {
            self.open_workspace_overview();
        }
    }

    fn open_workspace_overview(&self) {
        let interrupted_zoom = self.pane_zoom.transition.borrow().is_some();
        self.cancel_pane_zoom_transition();
        if interrupted_zoom {
            // Cancellation reparents the real pane. Allocate it before taking
            // the preview, rather than capturing its invalidated old bounds.
            self.stack
                .allocate(self.stack.width(), self.stack.height(), -1, None);
        }
        let active_workspace = self.sidebar.selected_workspace();
        let titles = self.sidebar.workspace_titles().borrow().clone();
        let (window_width, window_height) =
            (self.content_overlay.width(), self.content_overlay.height());
        let zoomed_preview = self.pane_zoom.active.borrow().as_ref().and_then(|zoom| {
            workspace_preview_texture(&zoom.frame, window_width, window_height)
                .map(|(texture, _, _)| (zoom.workspace, texture))
        });
        let entries = {
            let surfaces = self.surfaces.borrow();
            titles
                .into_iter()
                .map(|(workspace, name)| WorkspaceOverviewEntry {
                    workspace,
                    name,
                    texture: zoomed_preview
                        .as_ref()
                        .filter(|(zoomed_workspace, _)| *zoomed_workspace == workspace)
                        .map(|(_, texture)| texture.clone())
                        .or_else(|| {
                            surfaces.get(&workspace).and_then(|surface| {
                                workspace_preview_texture(surface, window_width, window_height)
                                    .map(|(texture, _, _)| texture)
                            })
                        }),
                })
                .collect::<Vec<_>>()
        };

        let top_margin = self
            .sidebar
            .header
            .compute_bounds(&self.content_overlay)
            .map(|bounds| (bounds.y() + bounds.height()).ceil() as i32)
            .unwrap_or_else(|| self.sidebar.header.height())
            .max(0);
        let controller_for_activate = self.clone();
        let activate = Rc::new(move |workspace| {
            controller_for_activate.select_workspace_from_overview(workspace);
        });
        let controller_for_close = self.clone();
        let close = Rc::new(move |workspace| {
            controller_for_close.close_workspace_from_overview(workspace);
        });
        let controller_for_dismiss = self.clone();
        let dismiss = Rc::new(move || controller_for_dismiss.close_workspace_overview(None));
        let view = build_workspace_overview_view(
            entries,
            active_workspace,
            top_margin,
            activate,
            close,
            dismiss,
        );
        let saved_focus =
            gtk::prelude::GtkWindowExt::focus(&self.window).map(|widget| widget.downgrade());
        let saved_window_title = self.window.title().map(|title| title.to_string());
        let native_views_suspend = crate::ui::browser_pane::suspend_native_browser_views_for_window(
            self.window.upcast_ref(),
        );

        self.window.set_title(Some(WORKSPACE_OVERVIEW_WINDOW_TITLE));
        self.content_overlay.add_overlay(&view.root);
        // Capture Escape during entry too, before the card receives focus.
        view.root.grab_focus();
        self.workspace_overview
            .active
            .replace(Some(ActiveWorkspaceOverview {
                root: view.root.clone(),
                chrome: view.chrome.clone(),
                transition_layer: view.transition_layer,
                title: view.title,
                flow: view.flow,
                cards: view.cards,
                active_workspace,
                saved_focus,
                saved_window_title,
                _native_views_suspend: native_views_suspend,
            }));

        if !adw::is_animations_enabled(&view.root) {
            self.finish_workspace_overview_open();
            return;
        }

        self.workspace_overview.transitioning.set(true);
        let controller = self.clone();
        // Wait for layout before reading the new card's bounds. An idle can
        // run before allocation, causing the opening animation to be skipped.
        let generation = self.workspace_overview.next_generation();
        let primed = Cell::new(false);
        let tick = view.root.add_tick_callback(move |_, _| {
            if !primed.replace(true) {
                return glib::ControlFlow::Continue;
            }
            if controller.workspace_overview.generation.get() == generation {
                controller.workspace_overview.tick.borrow_mut().take();
                controller.start_workspace_overview_open_animation();
            }
            glib::ControlFlow::Break
        });
        *self.workspace_overview.tick.borrow_mut() = Some(tick);
    }

    fn start_workspace_overview_open_animation(&self) {
        let Some((root, chrome, layer, card)) = self
            .workspace_overview
            .active
            .borrow()
            .as_ref()
            .and_then(|active| {
                active.active_workspace.and_then(|workspace| {
                    active
                        .cards
                        .iter()
                        .find(|card| card.workspace == workspace && card.texture.is_some())
                        .cloned()
                        .map(|card| {
                            (
                                active.root.clone(),
                                active.chrome.clone(),
                                active.transition_layer.clone(),
                                card,
                            )
                        })
                })
            })
        else {
            self.finish_workspace_overview_open();
            return;
        };
        let Some(source) = self
            .stack
            .compute_bounds(&root)
            .and_then(WindowMoveRect::from_bounds)
        else {
            self.finish_workspace_overview_open();
            return;
        };
        let Some(target) = card
            .picture
            .compute_bounds(&root)
            .and_then(WindowMoveRect::from_bounds)
        else {
            self.finish_workspace_overview_open();
            return;
        };
        let Some(texture) = card.texture.clone() else {
            self.finish_workspace_overview_open();
            return;
        };

        card.picture.set_opacity(0.0);
        let moving = transition_picture(&texture);
        let generation = self.workspace_overview.next_generation();
        let controller = self.clone();
        animate_workspace_picture(
            &layer,
            &moving,
            source,
            target,
            generation,
            self.workspace_overview.generation.clone(),
            self.workspace_overview.tick.clone(),
            move |progress| chrome.set_opacity(f64::from(progress)),
            move || {
                card.picture.set_opacity(1.0);
                controller.finish_workspace_overview_open();
            },
        );
    }

    fn finish_workspace_overview_open(&self) {
        let focus = self
            .workspace_overview
            .active
            .borrow()
            .as_ref()
            .and_then(|active| {
                active.chrome.set_opacity(1.0);
                active
                    .active_workspace
                    .and_then(|workspace| {
                        active.cards.iter().find(|card| card.workspace == workspace)
                    })
                    .or_else(|| active.cards.first())
                    .map(|card| card.button.clone())
            });
        self.workspace_overview.transitioning.set(false);
        if let Some(focus) = focus {
            focus.grab_focus();
        }
        if self.workspace_overview.dismiss_pending.replace(false) {
            self.close_workspace_overview(None);
        }
    }

    fn close_workspace_overview(&self, selected_workspace: Option<WorkspaceId>) {
        if self.workspace_overview.transitioning.get() {
            self.workspace_overview.dismiss_pending.set(true);
            return;
        }
        let selected_workspace =
            selected_workspace.filter(|workspace| self.surfaces.borrow().contains_key(workspace));
        let Some((root, chrome, layer, card)) = self
            .workspace_overview
            .active
            .borrow()
            .as_ref()
            .and_then(|active| {
                selected_workspace
                    .or(active.active_workspace)
                    .and_then(|workspace| {
                        active
                            .cards
                            .iter()
                            .find(|card| card.workspace == workspace && card.texture.is_some())
                            .cloned()
                    })
                    .map(|card| {
                        (
                            active.root.clone(),
                            active.chrome.clone(),
                            active.transition_layer.clone(),
                            card,
                        )
                    })
            })
        else {
            self.finish_workspace_overview_close(selected_workspace);
            return;
        };
        if !adw::is_animations_enabled(&root) {
            self.finish_workspace_overview_close(selected_workspace);
            return;
        }
        let Some(source) = card
            .picture
            .compute_bounds(&root)
            .and_then(WindowMoveRect::from_bounds)
        else {
            self.finish_workspace_overview_close(selected_workspace);
            return;
        };
        let Some(target) = self
            .stack
            .compute_bounds(&root)
            .and_then(WindowMoveRect::from_bounds)
        else {
            self.finish_workspace_overview_close(selected_workspace);
            return;
        };
        let Some(texture) = card.texture.clone() else {
            self.finish_workspace_overview_close(selected_workspace);
            return;
        };

        self.workspace_overview.transitioning.set(true);
        card.picture.set_opacity(0.0);
        let moving = transition_picture(&texture);
        let generation = self.workspace_overview.next_generation();
        let controller = self.clone();
        animate_workspace_picture(
            &layer,
            &moving,
            source,
            target,
            generation,
            self.workspace_overview.generation.clone(),
            self.workspace_overview.tick.clone(),
            move |progress| chrome.set_opacity(f64::from(1.0 - progress)),
            move || controller.finish_workspace_overview_close(selected_workspace),
        );
    }

    fn select_workspace_from_overview(&self, workspace: WorkspaceId) {
        if self.workspace_overview.transitioning.get()
            || !self.surfaces.borrow().contains_key(&workspace)
        {
            return;
        }
        self.workspace_overview.transitioning.set(true);
        let controller = self.clone();
        glib::MainContext::default().spawn_local(async move {
            if controller.sidebar.selected_workspace() != Some(workspace) {
                controller.activate_workspace(workspace).await;
            }
            controller.workspace_overview.transitioning.set(false);
            if controller.workspace_overview.is_active() {
                controller.close_workspace_overview(Some(workspace));
            }
        });
    }

    fn close_workspace_from_overview(&self, workspace: WorkspaceId) {
        if self.workspace_overview.transitioning.get()
            || !self.surfaces.borrow().contains_key(&workspace)
        {
            return;
        }
        self.workspace_overview.transitioning.set(true);
        let controller = self.clone();
        glib::MainContext::default().spawn_local(async move {
            if !controller.confirm_close_workspace(workspace).await {
                controller.workspace_overview.transitioning.set(false);
                return;
            }
            let (ack, _rx) = oneshot::channel();
            controller
                .dispatch_workspace_command(GtkCommand::RemoveWorkspace {
                    id: workspace,
                    confirm: false,
                    ack,
                })
                .await;
            controller.workspace_overview.transitioning.set(false);
            if !controller.surfaces.borrow().contains_key(&workspace) {
                controller.remove_workspace_overview_card(workspace);
            }
        });
    }

    fn remove_workspace_overview_card(&self, workspace: WorkspaceId) {
        let (focus, empty) = {
            let mut overview = self.workspace_overview.active.borrow_mut();
            let Some(active) = overview.as_mut() else {
                return;
            };
            let Some(index) = active
                .cards
                .iter()
                .position(|card| card.workspace == workspace)
            else {
                return;
            };
            let removed = active.cards.remove(index);
            active.flow.remove(&removed.root);
            active
                .title
                .set_text(&workspace_count_label(active.cards.len()));
            active.active_workspace = self.sidebar.selected_workspace();
            for card in &active.cards {
                if Some(card.workspace) == active.active_workspace {
                    card.button.add_css_class("active");
                } else {
                    card.button.remove_css_class("active");
                }
            }
            let focus = active
                .active_workspace
                .and_then(|selected| active.cards.iter().find(|card| card.workspace == selected))
                .or_else(|| {
                    active
                        .cards
                        .get(index.min(active.cards.len().saturating_sub(1)))
                })
                .map(|card| card.button.clone());
            (focus, active.cards.is_empty())
        };
        if empty {
            self.dismiss_workspace_overview_immediately();
            let controller = self.clone();
            glib::MainContext::default()
                .spawn_local(async move { controller.refresh_window_title().await });
        } else if let Some(focus) = focus {
            focus.grab_focus();
        }
    }

    fn finish_workspace_overview_close(&self, selected_workspace: Option<WorkspaceId>) {
        let saved_focus = self.remove_workspace_overview();
        if let Some(workspace) = selected_workspace {
            let controller = self.clone();
            glib::MainContext::default().spawn_local(async move {
                if controller.sidebar.selected_workspace() != Some(workspace) {
                    controller.activate_workspace(workspace).await;
                }
                if let Some(pane) = controller.focused_pane.get() {
                    controller.focus_pane(pane);
                }
                controller.refresh_window_title().await;
            });
        } else if let Some(saved_focus) = saved_focus.and_then(|focus| focus.upgrade()) {
            glib::idle_add_local_once(move || {
                saved_focus.grab_focus();
            });
        }
    }

    pub(super) fn dismiss_workspace_overview_immediately(&self) {
        self.remove_workspace_overview();
    }

    fn remove_workspace_overview(&self) -> Option<glib::WeakRef<gtk::Widget>> {
        self.workspace_overview.next_generation();
        self.workspace_overview.transitioning.set(false);
        self.workspace_overview.dismiss_pending.set(false);
        if let Some(tick) = self.workspace_overview.tick.borrow_mut().take() {
            tick.remove();
        }
        let active = self.workspace_overview.active.borrow_mut().take()?;
        if active.root.parent().as_ref() == Some(self.content_overlay.upcast_ref()) {
            self.content_overlay.remove_overlay(&active.root);
        }
        self.window.set_title(active.saved_window_title.as_deref());
        active.saved_focus
    }
}

fn build_workspace_overview_view(
    entries: Vec<WorkspaceOverviewEntry>,
    active_workspace: Option<WorkspaceId>,
    top_margin: i32,
    activate: Rc<dyn Fn(WorkspaceId)>,
    close: Rc<dyn Fn(WorkspaceId)>,
    dismiss: Rc<dyn Fn()>,
) -> WorkspaceOverviewView {
    let root = gtk::Overlay::new();
    root.set_focusable(true);
    root.set_halign(gtk::Align::Fill);
    root.set_valign(gtk::Align::Fill);
    root.set_hexpand(true);
    root.set_vexpand(true);
    root.set_margin_top(top_margin);

    let chrome = gtk::Box::new(gtk::Orientation::Vertical, 20);
    chrome.add_css_class("flowmux-workspace-overview");
    chrome.set_halign(gtk::Align::Fill);
    chrome.set_valign(gtk::Align::Fill);
    chrome.set_hexpand(true);
    chrome.set_vexpand(true);
    chrome.set_opacity(0.0);

    let title = gtk::Label::new(Some(&workspace_count_label(entries.len())));
    title.add_css_class("title-2");
    title.add_css_class("flowmux-workspace-overview-title");
    chrome.append(&title);

    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .row_spacing(24)
        .column_spacing(24)
        .min_children_per_line(1)
        .max_children_per_line(5)
        .build();
    flow.add_css_class("flowmux-workspace-overview-grid");
    flow.set_halign(gtk::Align::Center);
    flow.set_valign(gtk::Align::Start);

    let mut cards = Vec::with_capacity(entries.len());
    for entry in entries {
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_shrink(true);
        let (thumbnail_width, thumbnail_height) = entry
            .texture
            .as_ref()
            .map(|texture| workspace_thumbnail_size(texture.width(), texture.height()))
            .unwrap_or((THUMBNAIL_MAX_WIDTH, 180));
        picture.set_size_request(thumbnail_width, thumbnail_height);
        picture.set_alternative_text(Some(&format!("{} workspace preview", entry.name)));
        if let Some(texture) = entry.texture.as_ref() {
            picture.set_paintable(Some(texture));
        }
        picture.add_css_class("flowmux-workspace-overview-picture");

        let name = gtk::Label::new(Some(&entry.name));
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.set_max_width_chars(36);
        name.add_css_class("flowmux-workspace-overview-name");

        let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
        body.append(&picture);
        body.append(&name);

        let button = gtk::Button::new();
        button.set_child(Some(&body));
        button.add_css_class("flat");
        button.add_css_class("flowmux-workspace-overview-card");
        button.set_tooltip_text(Some(&format!("Open workspace {}", entry.name)));
        button.update_property(&[gtk::accessible::Property::Label(&format!(
            "Open workspace {}",
            entry.name
        ))]);
        if active_workspace == Some(entry.workspace) {
            button.add_css_class("active");
        }
        let workspace = entry.workspace;
        let activate_workspace = activate.clone();
        button.connect_clicked(move |_| activate_workspace(workspace));

        let close_button = gtk::Button::from_icon_name("window-close-symbolic");
        close_button.add_css_class("flat");
        close_button.add_css_class("circular");
        close_button.add_css_class("flowmux-workspace-overview-close");
        close_button.set_tooltip_text(Some(&format!("Close workspace {}", entry.name)));
        close_button.update_property(&[gtk::accessible::Property::Label(&format!(
            "Close workspace {}",
            entry.name
        ))]);
        close_button.set_halign(gtk::Align::End);
        close_button.set_valign(gtk::Align::Start);
        close_button.set_margin_top(12);
        close_button.set_margin_end(12);
        close_button.set_focusable(false);
        let close_workspace = close.clone();
        close_button.connect_clicked(move |_| close_workspace(workspace));

        let card_root = gtk::Overlay::new();
        card_root.set_child(Some(&button));
        card_root.add_overlay(&close_button);
        flow.append(&card_root);
        if let Some(child) = card_root.parent() {
            child.set_focusable(false);
        }
        cards.push(WorkspaceOverviewCard {
            workspace,
            root: card_root,
            button,
            picture,
            texture: entry.texture,
        });
    }

    if cards.is_empty() {
        let empty = gtk::Label::new(Some("No workspaces"));
        empty.add_css_class("dim-label");
        empty.set_vexpand(true);
        empty.set_valign(gtk::Align::Center);
        chrome.append(&empty);
    } else {
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .vexpand(true)
            .child(&flow)
            .build();
        chrome.append(&scroll);
    }

    let buttons = cards
        .iter()
        .map(|card| card.button.clone())
        .collect::<Vec<_>>();
    let root_for_key = root.downgrade();
    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    key.connect_key_pressed(move |_, keyval, _, _| {
        let Some(root) = root_for_key.upgrade() else {
            return glib::Propagation::Proceed;
        };
        handle_workspace_overview_key(&root, &buttons, keyval, dismiss.as_ref())
    });
    root.add_controller(key);
    root.set_child(Some(&chrome));

    let transition_layer = gtk::Fixed::new();
    transition_layer.set_can_target(false);
    transition_layer.set_halign(gtk::Align::Fill);
    transition_layer.set_valign(gtk::Align::Fill);
    transition_layer.set_hexpand(true);
    transition_layer.set_vexpand(true);
    root.add_overlay(&transition_layer);

    WorkspaceOverviewView {
        root,
        chrome,
        transition_layer,
        title,
        flow,
        cards,
    }
}

fn workspace_overview_dismisses_for_key(keyval: gtk::gdk::Key) -> bool {
    keyval == gtk::gdk::Key::Escape
}

fn workspace_overview_direction(keyval: gtk::gdk::Key) -> Option<gtk::DirectionType> {
    match keyval {
        gtk::gdk::Key::Left | gtk::gdk::Key::KP_Left => Some(gtk::DirectionType::Left),
        gtk::gdk::Key::Right | gtk::gdk::Key::KP_Right => Some(gtk::DirectionType::Right),
        gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up => Some(gtk::DirectionType::Up),
        gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down => Some(gtk::DirectionType::Down),
        _ => None,
    }
}

fn workspace_overview_activates_for_key(keyval: gtk::gdk::Key) -> bool {
    matches!(
        keyval,
        gtk::gdk::Key::Return | gtk::gdk::Key::ISO_Enter | gtk::gdk::Key::KP_Enter
    )
}

fn handle_workspace_overview_key(
    root: &gtk::Overlay,
    buttons: &[gtk::Button],
    keyval: gtk::gdk::Key,
    dismiss: &dyn Fn(),
) -> glib::Propagation {
    if workspace_overview_dismisses_for_key(keyval) {
        dismiss();
        return glib::Propagation::Stop;
    }
    if workspace_overview_activates_for_key(keyval) {
        if let Some(button) = focused_workspace_button(root, buttons) {
            button.emit_clicked();
        }
        return glib::Propagation::Stop;
    }
    let Some(direction) = workspace_overview_direction(keyval) else {
        return glib::Propagation::Proceed;
    };
    let had_card_focus = focused_workspace_button(root, buttons).is_some();
    if root.child_focus(direction) {
        if let Some(button) = focused_workspace_button(root, buttons) {
            button.grab_focus();
        }
    } else if !had_card_focus {
        if let Some(first) = buttons.first() {
            first.grab_focus();
        }
    }
    glib::Propagation::Stop
}

fn focused_workspace_button(root: &gtk::Overlay, buttons: &[gtk::Button]) -> Option<gtk::Button> {
    let window = root.root()?.downcast::<gtk::Window>().ok()?;
    let focus = gtk::prelude::GtkWindowExt::focus(&window)?;
    buttons
        .iter()
        .find(|button| {
            button.upcast_ref::<gtk::Widget>() == &focus
                || button.is_ancestor(&focus)
                || focus.is_ancestor(button.upcast_ref::<gtk::Widget>())
        })
        .cloned()
}

fn workspace_thumbnail_size(width: i32, height: i32) -> (i32, i32) {
    let width = f64::from(width.max(1));
    let height = f64::from(height.max(1));
    let scale =
        (f64::from(THUMBNAIL_MAX_WIDTH) / width).min(f64::from(THUMBNAIL_MAX_HEIGHT) / height);
    (
        (width * scale).round().max(1.0) as i32,
        (height * scale).round().max(1.0) as i32,
    )
}

fn workspace_count_label(count: usize) -> String {
    format!(
        "{count} {}",
        if count == 1 {
            "Workspace"
        } else {
            "Workspaces"
        }
    )
}

fn transition_picture(texture: &gtk::gdk::Texture) -> gtk::Picture {
    let picture = gtk::Picture::new();
    picture.set_paintable(Some(texture));
    picture.set_content_fit(gtk::ContentFit::Fill);
    picture.set_can_shrink(true);
    picture
}

#[allow(clippy::too_many_arguments)]
fn animate_workspace_picture(
    layer: &gtk::Fixed,
    picture: &gtk::Picture,
    source: WindowMoveRect,
    target: WindowMoveRect,
    generation: u64,
    current_generation: Rc<Cell<u64>>,
    current_tick: Rc<RefCell<Option<gtk::TickCallbackId>>>,
    progress_changed: impl Fn(f32) + 'static,
    finish: impl FnOnce() + 'static,
) {
    let base = WindowMoveRect {
        x: 0.0,
        y: 0.0,
        width: source.width.max(target.width),
        height: source.height.max(target.height),
    };
    picture.set_halign(gtk::Align::Start);
    picture.set_valign(gtk::Align::Start);
    picture.set_size_request(base.width.round() as i32, base.height.round() as i32);
    layer.put(picture, 0.0, 0.0);
    set_window_move_widget_rect(layer, picture.upcast_ref(), base, source);

    let layer = layer.clone();
    let picture = picture.clone();
    let primed = Cell::new(false);
    let started_at = Cell::new(None::<i64>);
    let finish = Rc::new(RefCell::new(Some(finish)));
    let duration_micros = WINDOW_MOVE_ANIMATION_DURATION.as_micros() as f32;
    let tick_for_finish = current_tick.clone();
    let tick = layer.clone().add_tick_callback(move |layer, clock| {
        if current_generation.get() != generation {
            return glib::ControlFlow::Break;
        }
        if !primed.replace(true) {
            return glib::ControlFlow::Continue;
        }
        let start = started_at.get().unwrap_or_else(|| {
            let now = clock.frame_time();
            started_at.set(Some(now));
            now
        });
        let elapsed = clock.frame_time().saturating_sub(start) as f32;
        let progress = (elapsed / duration_micros).clamp(0.0, 1.0);
        set_window_move_widget_rect(
            layer,
            picture.upcast_ref(),
            base,
            source.interpolate(target, progress),
        );
        progress_changed(progress);
        if progress < 1.0 {
            return glib::ControlFlow::Continue;
        }
        if picture.parent().as_ref() == Some(layer.upcast_ref()) {
            layer.remove(&picture);
        }
        tick_for_finish.borrow_mut().take();
        if let Some(finish) = finish.borrow_mut().take() {
            finish();
        }
        glib::ControlFlow::Break
    });
    *current_tick.borrow_mut() = Some(tick);
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_state::State;

    fn texture_bytes(texture: &gtk::gdk::Texture) -> Vec<u8> {
        let stride = texture.width() as usize * 4;
        let mut bytes = vec![0; stride * texture.height() as usize];
        gtk::gdk::prelude::TextureExtManual::download(texture, &mut bytes, stride);
        bytes
    }

    #[cfg(not(target_os = "macos"))]
    async fn wait_for_overview_transition(controller: &WindowController) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while controller.workspace_overview.transitioning.get() {
            assert!(
                Instant::now() < deadline,
                "workspace overview transition timed out"
            );
            glib::timeout_future(Duration::from_millis(5)).await;
        }
    }

    fn save_overview_snapshot(
        content_overlay: &gtk::Overlay,
        root: &gtk::Overlay,
        path: &std::path::Path,
    ) {
        let renderer = root.native().unwrap().renderer().unwrap();
        let snapshot = gtk::Snapshot::new();
        let paintable = gtk::WidgetPaintable::new(Some(content_overlay));
        paintable.snapshot(
            &snapshot,
            f64::from(content_overlay.width()),
            f64::from(content_overlay.height()),
        );
        let node = snapshot.to_node().unwrap();
        let viewport = gtk::graphene::Rect::new(
            0.0,
            0.0,
            content_overlay.width() as f32,
            content_overlay.height() as f32,
        );
        renderer
            .render_texture(&node, Some(&viewport))
            .save_to_png(path)
            .unwrap();
    }

    #[test]
    fn workspace_count_uses_project_terminology_and_correct_plural() {
        assert_eq!(workspace_count_label(0), "0 Workspaces");
        assert_eq!(workspace_count_label(1), "1 Workspace");
        assert_eq!(workspace_count_label(7), "7 Workspaces");
    }

    #[test]
    fn only_escape_dismisses_from_the_overview_key_handler() {
        assert!(workspace_overview_dismisses_for_key(gtk::gdk::Key::Escape));
        assert!(!workspace_overview_dismisses_for_key(gtk::gdk::Key::Return));
        assert!(!workspace_overview_dismisses_for_key(gtk::gdk::Key::k));
        assert!(workspace_overview_activates_for_key(gtk::gdk::Key::Return));
        assert!(workspace_overview_activates_for_key(
            gtk::gdk::Key::KP_Enter
        ));
        assert_eq!(
            workspace_overview_direction(gtk::gdk::Key::Left),
            Some(gtk::DirectionType::Left)
        );
        assert_eq!(
            workspace_overview_direction(gtk::gdk::Key::Down),
            Some(gtk::DirectionType::Down)
        );
    }

    #[test]
    fn background_updates_keep_overview_while_navigation_replaces_it() {
        assert!(!command_dismisses_workspace_overview(
            &GtkCommand::RefreshWindowTitle
        ));
        assert!(!command_dismisses_workspace_overview(
            &GtkCommand::ToggleWorkspaceOverview
        ));
        assert!(command_dismisses_workspace_overview(
            &GtkCommand::ActivateWorkspace {
                id: WorkspaceId::new(),
            }
        ));
        assert!(command_dismisses_workspace_overview(
            &GtkCommand::TogglePaneZoom {
                pane: PaneId::new(),
            }
        ));
    }

    #[test]
    fn workspace_thumbnail_preserves_landscape_and_portrait_aspect_ratios() {
        assert_eq!(workspace_thumbnail_size(1600, 900), (320, 180));
        assert_eq!(workspace_thumbnail_size(900, 1200), (150, 200));
        assert_eq!(workspace_thumbnail_size(0, 0), (200, 200));
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    fn view_preserves_workspace_order_active_state_and_activation_target() {
        gtk::init().expect("GTK should initialize in GTK test");
        let first = WorkspaceId::new();
        let second = WorkspaceId::new();
        let activated = Rc::new(Cell::new(None));
        let activated_for_click = activated.clone();
        let closed = Rc::new(Cell::new(None));
        let closed_for_click = closed.clone();
        let view = build_workspace_overview_view(
            vec![
                WorkspaceOverviewEntry {
                    workspace: first,
                    name: "first".into(),
                    texture: None,
                },
                WorkspaceOverviewEntry {
                    workspace: second,
                    name: "second".into(),
                    texture: None,
                },
            ],
            Some(second),
            0,
            Rc::new(move |workspace| activated_for_click.set(Some(workspace))),
            Rc::new(move |workspace| closed_for_click.set(Some(workspace))),
            Rc::new(|| {}),
        );

        assert_eq!(
            view.cards
                .iter()
                .map(|card| card.workspace)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(!view.cards[0].button.has_css_class("active"));
        assert!(view.cards[1].button.has_css_class("active"));
        assert_eq!(
            view.cards[1].button.tooltip_text().as_deref(),
            Some("Open workspace second")
        );
        view.cards[0].button.emit_clicked();
        assert_eq!(activated.get(), Some(first));
        let close_button = view.cards[1]
            .root
            .last_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        assert!(close_button.has_css_class("flowmux-workspace-overview-close"));
        assert_eq!(close_button.halign(), gtk::Align::End);
        assert_eq!(close_button.valign(), gtk::Align::Start);
        assert_eq!(
            close_button.tooltip_text().as_deref(),
            Some("Close workspace second")
        );
        close_button.emit_clicked();
        assert_eq!(closed.get(), Some(second));
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    async fn controller_overview_selects_workspace_and_cleans_up_before_other_commands() {
        adw::init().expect("libadwaita should initialize in GTK test");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let _runtime_guard = runtime.enter();
        let store = StateStore::new_lazy(State::default());
        let first = store
            .create_workspace(Some("first".into()), PathBuf::from("/tmp"))
            .await;
        let first_pane = store.get_workspace(first).await.unwrap().surfaces[0]
            .root_pane
            .first_leaf_id()
            .unwrap();
        store
            .split_pane(first_pane, SplitDirection::Vertical)
            .await
            .expect("the first workspace should split");
        let second = store
            .create_workspace(Some("second".into()), PathBuf::from("/tmp"))
            .await;
        let second_pane = store.get_workspace(second).await.unwrap().surfaces[0]
            .root_pane
            .first_leaf_id()
            .unwrap();
        let (bridge, _rx) = Bridge::new();
        let app = adw::Application::builder()
            .application_id("com.flowmux.App.UiTest.WorkspaceOverviewController")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let controller = WindowController::new(
            &app,
            store.clone(),
            Arc::new(ResolvedTheme::load()),
            bridge,
            gtk::CssProvider::new(),
            Some(runtime.handle().clone()),
        );
        controller.render_workspace(&store.get_workspace(first).await.unwrap());
        controller.render_workspace(&store.get_workspace(second).await.unwrap());
        controller.activate_workspace(first).await;
        controller.window.present();
        glib::timeout_future(Duration::from_millis(100)).await;

        controller.toggle_pane_zoom(first_pane);
        glib::timeout_future(WINDOW_MOVE_ANIMATION_DURATION + Duration::from_millis(100)).await;
        let zoomed_texture = {
            let zoom = controller.pane_zoom.active.borrow();
            let frame = &zoom.as_ref().expect("pane should be zoomed").frame;
            workspace_preview_texture(
                frame,
                controller.content_overlay.width(),
                controller.content_overlay.height(),
            )
            .expect("the zoomed pane should be capturable")
            .0
        };

        controller
            .dispatch(GtkCommand::ToggleWorkspaceOverview)
            .await;
        assert!(controller.workspace_overview.is_active());
        assert_eq!(
            controller.window.title().as_deref(),
            Some(WORKSPACE_OVERVIEW_WINDOW_TITLE)
        );
        let animations_enabled = adw::is_animations_enabled(&controller.content_overlay);
        assert_eq!(
            controller.workspace_overview.transitioning.get(),
            animations_enabled
        );
        wait_for_overview_transition(&controller).await;

        let (ids, active_classes, tooltips, textures_present, buttons, root) = {
            let active = controller.workspace_overview.active.borrow();
            let active = active.as_ref().expect("overview should stay open");
            (
                active
                    .cards
                    .iter()
                    .map(|card| card.workspace)
                    .collect::<Vec<_>>(),
                active
                    .cards
                    .iter()
                    .map(|card| card.button.has_css_class("active"))
                    .collect::<Vec<_>>(),
                active
                    .cards
                    .iter()
                    .map(|card| card.button.tooltip_text().unwrap().to_string())
                    .collect::<Vec<_>>(),
                active
                    .cards
                    .iter()
                    .map(|card| card.texture.is_some())
                    .collect::<Vec<_>>(),
                active
                    .cards
                    .iter()
                    .map(|card| card.button.clone())
                    .collect::<Vec<_>>(),
                active.root.clone(),
            )
        };
        assert_eq!(ids, vec![first, second]);
        assert_eq!(active_classes, vec![true, false]);
        assert_eq!(
            tooltips,
            controller
                .sidebar
                .workspace_titles()
                .borrow()
                .iter()
                .map(|(_, name)| format!("Open workspace {name}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(textures_present, vec![true, true]);
        let overview_texture = {
            let active = controller.workspace_overview.active.borrow();
            active.as_ref().unwrap().cards[0].texture.clone().unwrap()
        };
        assert_eq!(
            texture_bytes(&overview_texture),
            texture_bytes(&zoomed_texture),
            "the zoomed pane must remain visible in its overview card"
        );
        assert!(
            controller.zoomed_pane() == Some(first_pane),
            "overview must preserve zoom so its preview matches the returning workspace"
        );
        assert!(!controller.workspace_overview.transitioning.get());
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&controller.window).as_ref(),
            Some(buttons[0].upcast_ref())
        );
        assert_eq!(
            root.parent().as_ref(),
            Some(controller.content_overlay.upcast_ref())
        );
        let header_bounds = controller
            .sidebar
            .header
            .compute_bounds(&controller.content_overlay)
            .unwrap();
        let root_bounds = root.compute_bounds(&controller.content_overlay).unwrap();
        let chrome_bounds = root
            .child()
            .unwrap()
            .compute_bounds(&controller.content_overlay)
            .unwrap();
        assert!(
            (root_bounds.y() - (header_bounds.y() + header_bounds.height())).abs() < 1.0,
            "overview must start below the retained window bar"
        );
        assert!(root_bounds.x().abs() < 1.0);
        assert!((root_bounds.width() - controller.content_overlay.width() as f32).abs() < 1.0);
        assert!((chrome_bounds.x() - root_bounds.x()).abs() < 1.0);
        assert!((chrome_bounds.width() - root_bounds.width()).abs() < 1.0);
        if let Some(path) = std::env::var_os("FLOWMUX_TEST_OVERVIEW_SCREENSHOT") {
            save_overview_snapshot(
                &controller.content_overlay,
                &root,
                std::path::Path::new(&path),
            );
        }

        assert_eq!(
            handle_workspace_overview_key(&root, &buttons, gtk::gdk::Key::Right, &|| {}),
            glib::Propagation::Stop
        );
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&controller.window).as_ref(),
            Some(buttons[1].upcast_ref())
        );
        handle_workspace_overview_key(&root, &buttons, gtk::gdk::Key::Right, &|| {});
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&controller.window).as_ref(),
            Some(buttons[1].upcast_ref()),
            "directional focus must stay on the edge card"
        );
        handle_workspace_overview_key(&root, &buttons, gtk::gdk::Key::Left, &|| {});
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&controller.window).as_ref(),
            Some(buttons[0].upcast_ref()),
            "left must return focus to the previous card"
        );
        handle_workspace_overview_key(&root, &buttons, gtk::gdk::Key::Right, &|| {});
        assert_eq!(
            handle_workspace_overview_key(&root, &buttons, gtk::gdk::Key::Return, &|| {}),
            glib::Propagation::Stop
        );
        if animations_enabled {
            assert!(controller.workspace_overview.is_active());
            assert!(controller.workspace_overview.transitioning.get());
        }
        glib::timeout_future(Duration::from_millis(20)).await;
        if animations_enabled {
            assert!(controller.workspace_overview.is_active());
            assert_eq!(
                controller.stack.visible_child_name().as_deref(),
                Some(second.to_string().as_str()),
                "the selected workspace must be underneath the closing animation"
            );
            assert_eq!(
                controller.window.title().as_deref(),
                Some(WORKSPACE_OVERVIEW_WINDOW_TITLE)
            );
        }
        wait_for_overview_transition(&controller).await;
        assert!(!controller.workspace_overview.is_active());
        assert_eq!(store.snapshot().await.active_workspace, Some(second));
        assert_eq!(controller.sidebar.selected_workspace(), Some(second));
        assert_eq!(
            controller.stack.visible_child_name().as_deref(),
            Some(second.to_string().as_str())
        );
        assert_eq!(controller.focused_pane.get(), Some(second_pane));
        assert_ne!(
            controller.window.title().as_deref(),
            Some(WORKSPACE_OVERVIEW_WINDOW_TITLE)
        );

        let title_before_second_overview = controller.window.title().map(|title| title.to_string());
        controller
            .dispatch(GtkCommand::ToggleWorkspaceOverview)
            .await;
        wait_for_overview_transition(&controller).await;
        assert!(controller.workspace_overview.is_active());
        controller.dispatch(GtkCommand::RefreshWindowTitle).await;
        assert!(
            controller.workspace_overview.is_active(),
            "background display updates must not dismiss the overview"
        );
        assert_eq!(
            controller.window.title().as_deref(),
            Some(WORKSPACE_OVERVIEW_WINDOW_TITLE)
        );
        controller.remove_workspace_overview_card(first);
        let surviving_button = {
            let active = controller.workspace_overview.active.borrow();
            let active = active.as_ref().unwrap();
            assert_eq!(active.cards.len(), 1);
            assert_eq!(active.cards[0].workspace, second);
            assert_eq!(active.title.text().as_str(), "1 Workspace");
            assert_eq!(active.flow.child_at_index(1), None);
            active.cards[0].button.clone()
        };
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&controller.window).as_ref(),
            Some(surviving_button.upcast_ref()),
            "closing a card keeps overview focus on the surviving workspace"
        );
        controller
            .dispatch(GtkCommand::ActivateWorkspace { id: second })
            .await;
        assert!(!controller.workspace_overview.is_active());
        assert_eq!(store.snapshot().await.active_workspace, Some(second));
        assert_eq!(
            controller.window.title().map(|title| title.to_string()),
            title_before_second_overview
        );

        controller.toggle_workspace_overview();
        let root = controller
            .workspace_overview
            .active
            .borrow()
            .as_ref()
            .unwrap()
            .root
            .clone();
        if animations_enabled {
            assert_eq!(
                gtk::prelude::GtkWindowExt::focus(&controller.window).as_ref(),
                Some(root.upcast_ref()),
                "Escape must reach overview before the first animation frame"
            );
        }
        controller.close_workspace_overview(None);
        wait_for_overview_transition(&controller).await;
        assert!(
            !controller.workspace_overview.is_active(),
            "entry must honor an immediate dismiss request"
        );
        drop(root);

        // Cancel before the first frame and during a running animation. Both
        // callbacks used to retain the detached overlay and controller.
        for delay in [0, 80] {
            controller.toggle_workspace_overview();
            let root = controller
                .workspace_overview
                .active
                .borrow()
                .as_ref()
                .unwrap()
                .root
                .downgrade();
            if delay > 0 {
                glib::timeout_future(Duration::from_millis(delay)).await;
            }
            controller.dismiss_workspace_overview_immediately();
            assert!(controller.workspace_overview.tick.borrow().is_none());
            assert!(
                root.upgrade().is_none(),
                "cancelling overview must release its widget graph"
            );
            assert!(!controller.workspace_overview.transitioning.get());
        }

        if animations_enabled {
            controller.activate_workspace(first).await;
            glib::timeout_future(Duration::from_millis(100)).await;
            let frame = controller
                .pane_registry
                .borrow()
                .pane_frame(first_pane)
                .unwrap();
            let original_width = frame.width();
            controller.toggle_pane_zoom(first_pane);
            let overlay = controller.content_overlay.last_child().unwrap().downgrade();
            glib::timeout_future(Duration::from_millis(80)).await;
            assert_eq!(
                frame.width(),
                original_width,
                "zoom must not reflow the live pane mid-animation"
            );
            controller.clear_pane_zoom();
            assert!(controller.pane_zoom.transition.borrow().is_none());
            assert!(
                overlay.upgrade().is_none(),
                "cancelling zoom must remove its tick callback and overlay"
            );
            assert_eq!(controller.zoomed_pane(), None);
        }
    }
}
