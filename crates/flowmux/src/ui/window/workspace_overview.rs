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
}

struct ActiveWorkspaceOverview {
    root: gtk::Overlay,
    chrome: gtk::Box,
    transition_layer: gtk::Fixed,
    cards: Vec<WorkspaceOverviewCard>,
    active_workspace: Option<WorkspaceId>,
    saved_focus: Option<glib::WeakRef<gtk::Widget>>,
    _native_views_suspend: crate::ui::browser_pane::NativeBrowserViewsSuspend,
}

#[derive(Clone)]
struct WorkspaceOverviewCard {
    workspace: WorkspaceId,
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
            return;
        }
        if self.workspace_overview.is_active() {
            self.close_workspace_overview(None);
        } else {
            self.open_workspace_overview();
        }
    }

    fn open_workspace_overview(&self) {
        self.clear_pane_zoom();

        let active_workspace = self.sidebar.selected_workspace();
        let titles = self.sidebar.workspace_titles().borrow().clone();
        let (window_width, window_height) =
            (self.content_overlay.width(), self.content_overlay.height());
        let entries = {
            let surfaces = self.surfaces.borrow();
            titles
                .into_iter()
                .map(|(workspace, name)| WorkspaceOverviewEntry {
                    workspace,
                    name,
                    texture: surfaces.get(&workspace).and_then(|surface| {
                        workspace_preview_texture(surface, window_width, window_height)
                            .map(|(texture, _, _)| texture)
                    }),
                })
                .collect::<Vec<_>>()
        };

        let controller_for_activate = self.clone();
        let activate = Rc::new(move |workspace| {
            controller_for_activate.close_workspace_overview(Some(workspace));
        });
        let controller_for_dismiss = self.clone();
        let dismiss = Rc::new(move || controller_for_dismiss.close_workspace_overview(None));
        let view = build_workspace_overview_view(entries, active_workspace, activate, dismiss);
        let saved_focus =
            gtk::prelude::GtkWindowExt::focus(&self.window).map(|widget| widget.downgrade());
        let native_views_suspend = crate::ui::browser_pane::suspend_native_browser_views_for_window(
            self.window.upcast_ref(),
        );

        self.content_overlay.add_overlay(&view.root);
        self.workspace_overview
            .active
            .replace(Some(ActiveWorkspaceOverview {
                root: view.root.clone(),
                chrome: view.chrome.clone(),
                transition_layer: view.transition_layer,
                cards: view.cards,
                active_workspace,
                saved_focus,
                _native_views_suspend: native_views_suspend,
            }));

        if !adw::is_animations_enabled(&view.root) {
            self.finish_workspace_overview_open();
            return;
        }

        self.workspace_overview.transitioning.set(true);
        let controller = self.clone();
        glib::idle_add_local_once(move || controller.start_workspace_overview_open_animation());
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
    }

    fn close_workspace_overview(&self, selected_workspace: Option<WorkspaceId>) {
        if self.workspace_overview.transitioning.get() {
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
            move |progress| chrome.set_opacity(f64::from(1.0 - progress)),
            move || controller.finish_workspace_overview_close(selected_workspace),
        );
    }

    fn finish_workspace_overview_close(&self, selected_workspace: Option<WorkspaceId>) {
        let saved_focus = self.remove_workspace_overview();
        if let Some(workspace) = selected_workspace {
            let controller = self.clone();
            glib::MainContext::default().spawn_local(async move {
                controller.activate_workspace(workspace).await;
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
        let active = self.workspace_overview.active.borrow_mut().take()?;
        if active.root.parent().as_ref() == Some(self.content_overlay.upcast_ref()) {
            self.content_overlay.remove_overlay(&active.root);
        }
        active.saved_focus
    }
}

fn build_workspace_overview_view(
    entries: Vec<WorkspaceOverviewEntry>,
    active_workspace: Option<WorkspaceId>,
    activate: Rc<dyn Fn(WorkspaceId)>,
    dismiss: Rc<dyn Fn()>,
) -> WorkspaceOverviewView {
    let root = gtk::Overlay::new();
    root.set_halign(gtk::Align::Fill);
    root.set_valign(gtk::Align::Fill);
    root.set_hexpand(true);
    root.set_vexpand(true);

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
        flow.append(&button);
        cards.push(WorkspaceOverviewCard {
            workspace,
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

    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    key.connect_key_pressed(move |_, keyval, _, _| {
        if workspace_overview_dismisses_for_key(keyval) {
            dismiss();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
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
        cards,
    }
}

fn workspace_overview_dismisses_for_key(keyval: gtk::gdk::Key) -> bool {
    keyval == gtk::gdk::Key::Escape
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
    layer.clone().add_tick_callback(move |layer, clock| {
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
        if let Some(finish) = finish.borrow_mut().take() {
            finish();
        }
        glib::ControlFlow::Break
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_state::State;

    fn save_overview_snapshot(
        content_overlay: &gtk::Overlay,
        root: &gtk::Overlay,
        path: &std::path::Path,
    ) {
        let renderer = root.native().unwrap().renderer().unwrap();
        let snapshot = gtk::Snapshot::new();
        content_overlay.snapshot_child(root, &snapshot);
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
            Rc::new(move |workspace| activated_for_click.set(Some(workspace))),
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

        controller
            .dispatch(GtkCommand::ToggleWorkspaceOverview)
            .await;
        assert!(controller.workspace_overview.is_active());
        let animations_enabled = adw::is_animations_enabled(&controller.content_overlay);
        assert_eq!(
            controller.workspace_overview.transitioning.get(),
            animations_enabled
        );
        glib::timeout_future(WINDOW_MOVE_ANIMATION_DURATION + Duration::from_millis(150)).await;

        let (ids, active_classes, tooltips, textures_present, first_button, second_button, root) = {
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
                active.cards[0].button.clone(),
                active
                    .cards
                    .iter()
                    .find(|card| card.workspace == second)
                    .unwrap()
                    .button
                    .clone(),
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
        assert!(!controller.workspace_overview.transitioning.get());
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&controller.window).as_ref(),
            Some(first_button.upcast_ref())
        );
        assert_eq!(
            root.parent().as_ref(),
            Some(controller.content_overlay.upcast_ref())
        );
        if let Some(path) = std::env::var_os("FLOWMUX_TEST_OVERVIEW_SCREENSHOT") {
            save_overview_snapshot(
                &controller.content_overlay,
                &root,
                std::path::Path::new(&path),
            );
        }

        second_button.emit_clicked();
        if animations_enabled {
            assert!(controller.workspace_overview.is_active());
            assert!(controller.workspace_overview.transitioning.get());
        }
        glib::timeout_future(WINDOW_MOVE_ANIMATION_DURATION + Duration::from_millis(200)).await;
        assert!(!controller.workspace_overview.is_active());
        assert_eq!(store.snapshot().await.active_workspace, Some(second));
        assert_eq!(controller.sidebar.selected_workspace(), Some(second));
        assert_eq!(
            controller.stack.visible_child_name().as_deref(),
            Some(second.to_string().as_str())
        );
        assert_eq!(controller.focused_pane.get(), Some(second_pane));

        controller
            .dispatch(GtkCommand::ToggleWorkspaceOverview)
            .await;
        glib::timeout_future(WINDOW_MOVE_ANIMATION_DURATION + Duration::from_millis(150)).await;
        assert!(controller.workspace_overview.is_active());
        controller.dispatch(GtkCommand::RefreshWindowTitle).await;
        assert!(
            controller.workspace_overview.is_active(),
            "background display updates must not dismiss the overview"
        );
        controller
            .dispatch(GtkCommand::ActivateWorkspace { id: second })
            .await;
        assert!(!controller.workspace_overview.is_active());
        assert_eq!(store.snapshot().await.active_workspace, Some(second));
    }
}
