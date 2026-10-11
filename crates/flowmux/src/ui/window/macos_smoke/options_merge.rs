// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_config::options::{self, AgentSortMode};

fn control(window: &gtk::Window, label: &str) -> gtk::Widget {
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

async fn dialog(controller: &WindowController) -> gtk::Window {
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
    let first_dialog = dialog(&first).await;
    let second_dialog = dialog(&second).await;
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
