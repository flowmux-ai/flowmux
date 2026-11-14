// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_config::options::{self, AgentSortMode};
use vte::prelude::*;

pub(super) fn control(window: &gtk::Window, label: &str) -> gtk::Widget {
    let mut stack = vec![window.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = stack.pop() {
        if widget
            .clone()
            .downcast::<gtk::Label>()
            .is_ok_and(|value| value.text() == label)
        {
            return widget.parent().unwrap().last_child().unwrap();
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            stack.push(widget);
        }
    }
    panic!("options control not found: {label}");
}

pub(super) async fn dialog(controller: &WindowController) -> gtk::Window {
    controller.dispatch(GtkCommand::ShowOptionsDialog).await;
    let dialog = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
        .find(|window| {
            window.widget_name() == "flowmux-options-dialog"
                && window.transient_for().as_ref() == Some(controller.window.upcast_ref())
        })
        .unwrap();
    wait_until("options dialog mapped", || dialog.is_mapped()).await;
    dialog
}

pub(super) async fn check(app: &adw::Application) {
    let make_window = || {
        let (bridge, _rx) = Bridge::new();
        let controller = WindowController::new(
            app,
            StateStore::new_lazy_ephemeral(State::default()),
            Arc::new(ResolvedTheme::load()),
            bridge,
            gtk::CssProvider::new(),
            None,
        );
        controller.window.present();
        controller
    };
    let first = make_window();
    let second = make_window();
    let mut pins = options::load().usage_bar_pins;
    pins.claude = Some(vec![]);
    first
        .dispatch(GtkCommand::SetUsageBarPins { pins: pins.clone() })
        .await;
    let first_dialog = dialog(&first).await;
    let second_dialog = dialog(&second).await;
    pins.codex = Some(vec![]);
    second
        .dispatch(GtkCommand::SetUsageBarPins { pins: pins.clone() })
        .await;
    control(&first_dialog, "Keep browser session data")
        .downcast::<gtk::CheckButton>()
        .unwrap()
        .set_active(false);
    wait_until("first window setting saved", || {
        !options::load().persist_browser_session
    })
    .await;
    second
        .dispatch(GtkCommand::SetAgentSortMode {
            mode: AgentSortMode::RecentlyFinished,
        })
        .await;
    assert!(
        !options::load().persist_browser_session,
        "second window reverted the first setting"
    );

    let usage = control(&second_dialog, "AI Usage bar")
        .downcast::<gtk::Switch>()
        .unwrap();
    usage.set_active(false);
    wait_until("stale dialog change saved", || {
        !options::load().usage_bar_enabled
    })
    .await;
    let zoom = control(&first_dialog, "Global zoom (%)")
        .downcast::<gtk::DropDown>()
        .unwrap();
    zoom.set_selected(zoom.model().unwrap().n_items() - 1);
    wait_until("first dialog zoom saved", || {
        options::load().zoom_percent == 200
    })
    .await;
    usage.set_active(true);
    wait_until("second dialog changed again", || {
        options::load().usage_bar_enabled
    })
    .await;
    let saved = options::load();
    assert_eq!(
        saved.usage_bar_pins, pins,
        "stale options dialogs must preserve pin selections"
    );
    assert_eq!(saved.zoom_percent, 200);
    assert!(!saved.persist_browser_session);
    assert_eq!(saved.agent_sort_mode, AgentSortMode::RecentlyFinished);
    assert_eq!(first.options.borrow().zoom_percent, 200);
    assert!(second.options.borrow().usage_bar_enabled);
    assert_eq!(
        second.options.borrow().agent_sort_mode,
        AgentSortMode::RecentlyFinished
    );
    first_dialog.close();
    second_dialog.close();
    first.window.destroy();
    second.window.destroy();
    println!("MACOS_OPTIONS_FIELD_MERGE_OK");
}

pub(super) async fn check_zoom(
    controller: &WindowController,
    workspace: WorkspaceId,
    pane: PaneId,
) {
    let original = controller
        .pane_registry
        .borrow()
        .active_terminal(pane)
        .unwrap()
        .clone();
    let assert_font = |terminal: &crate::ui::pane_terminal::PaneTerminal, points: f64| {
        assert_eq!(terminal.widget.font_scale(), 1.0);
        assert_eq!(
            terminal.widget.font().unwrap().size() as f64 / gtk::pango::SCALE as f64,
            points
        );
    };
    assert_font(&original, 12.0);
    let window = dialog(controller).await;
    let zoom = control(&window, "Global zoom (%)")
        .downcast::<gtk::DropDown>()
        .unwrap();
    let displayed = |zoom: &gtk::DropDown| {
        zoom.selected_item()
            .unwrap()
            .downcast::<gtk::StringObject>()
            .unwrap()
            .string()
            .to_string()
    };
    assert_eq!(displayed(&zoom), "75%");
    assert_eq!(
        control(&window, "Font size (pt)")
            .downcast::<gtk::SpinButton>()
            .unwrap()
            .value(),
        16.0
    );
    for (percent, points) in [(100, 16.0), (50, 8.0), (75, 12.0)] {
        let model = zoom.model().unwrap();
        let index = (0..model.n_items())
            .find(|index| {
                model
                    .item(*index)
                    .unwrap()
                    .downcast::<gtk::StringObject>()
                    .unwrap()
                    .string()
                    == format!("{percent}%")
            })
            .unwrap();
        zoom.set_selected(index);
        wait_until("zoom option saved", || {
            options::load().zoom_percent == percent
        })
        .await;
        assert_font(&original, points);
        wait_for_paint(&controller.window).await;
    }
    super::capture_native_window(&window, "zoom-75-font-16-options").await;
    window.close();
    let (_, surface) = controller
        .store
        .add_terminal_surface_to_pane(pane, None)
        .await
        .unwrap();
    controller
        .attach_or_rerender_surface(workspace, pane, surface, None)
        .await
        .unwrap();
    let new = controller
        .pane_registry
        .borrow()
        .active_terminal(pane)
        .unwrap()
        .clone();
    assert_font(&new, 12.0);
    assert_eq!(original.widget.char_height(), new.widget.char_height());
    super::capture_native_window(controller.window.upcast_ref(), "zoom-75-font-16-terminal").await;
    println!("MACOS_OPTIONS_ZOOM_FONT_OK saved=75 base_pt=16 terminal_pt=12 scale=1");

    let mut saved = options::load();
    saved.zoom_percent = 85;
    options::save(&saved).unwrap();
    *controller.options.borrow_mut() = saved.clone();
    controller.apply_runtime_theme(&saved);
    let window = dialog(controller).await;
    let zoom = control(&window, "Global zoom (%)")
        .downcast::<gtk::DropDown>()
        .unwrap();
    assert_eq!(
        displayed(&zoom),
        "85%",
        "opening options must show the applied zoom exactly"
    );
    control(&window, "Font size (pt)")
        .downcast::<gtk::SpinButton>()
        .unwrap()
        .set_value(18.0);
    wait_until("font option saved", || {
        options::load().font_size == Some(18.0)
    })
    .await;
    assert_eq!(
        options::load().zoom_percent,
        85,
        "editing font size must not change zoom"
    );
    assert_eq!(displayed(&zoom), "85%");
    assert_font(&original, 15.0);
    assert_font(&new, 15.0);
    window.close();
    println!("MACOS_OPTIONS_ZOOM_DISPLAY_OK");
}
