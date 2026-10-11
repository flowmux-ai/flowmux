// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(widget) = child {
        result.extend(descendants(&widget));
        child = widget.next_sibling();
    }
    result
}

fn buttons(window: &gtk::Window) -> Vec<(String, gtk::Button)> {
    descendants(window.upcast_ref())
        .into_iter()
        .filter_map(|widget| {
            let button = widget.downcast::<gtk::Button>().ok()?;
            if !button.is_visible() {
                return None;
            }
            let label = button
                .child()?
                .first_child()?
                .downcast::<gtk::Label>()
                .ok()?;
            Some((label.text().to_string(), button))
        })
        .collect()
}

async fn open(controller: &WindowController) -> (gtk::Window, gtk::SearchEntry) {
    let previous: Vec<_> = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .collect();
    controller.show_command_palette().await;
    let window = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|window| {
            !previous.contains(window)
                && window.is_visible()
                && window.title().as_deref() == Some("Command Palette")
                && window.transient_for().as_ref() == Some(controller.window.upcast_ref())
        })
        .unwrap();
    wait_until("palette mapped", || window.is_mapped()).await;
    let search = descendants(window.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::SearchEntry>().ok())
        .unwrap();
    (window, search)
}

async fn active(controller: &WindowController, expected: WorkspaceId) {
    glib::future_with_timeout(Duration::from_secs(10), async {
        while controller.store.active_or_first().await != Some(expected) {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("palette executed a different workspace command");
}

pub(super) async fn check(
    controller: &WindowController,
    original: WorkspaceId,
    root: &std::path::Path,
) {
    let target = controller
        .store
        .create_workspace(Some("Palette target".into()), root.into())
        .await;
    controller
        .store
        .rename_workspace(target, "Palette target".into())
        .await;
    controller
        .store
        .rename_workspace(original, "Palette original".into())
        .await;
    let label = "Workspace: Palette target";
    let (window, _) = open(controller).await;
    buttons(&window)
        .into_iter()
        .find(|(text, _)| text == label)
        .unwrap()
        .1
        .emit_clicked();
    active(controller, target).await;
    println!(
        "A17 recorded history: {:?}",
        controller.palette_mru.borrow()
    );
    controller.activate_workspace(original).await;
    let (window, search) = open(controller).await;
    assert_eq!(buttons(&window)[0].0, label);
    search.emit_activate();
    active(controller, target).await;

    let (window, search) = open(controller).await;
    search.set_text("Workspace: Palette original");
    search.emit_by_name::<()>("search-changed", &[]);
    assert_eq!(buttons(&window)[0].0, "Workspace: Palette original");
    search.emit_activate();
    active(controller, original).await;

    let (window, search) = open(controller).await;
    search.set_text("no-such-command-unique-a17");
    search.emit_by_name::<()>("search-changed", &[]);
    assert!(buttons(&window).is_empty());
    search.emit_activate();
    assert!(window.is_visible());
    assert_eq!(controller.store.active_or_first().await, Some(original));
    window.close();

    let file = root.join("palette.txt");
    std::fs::write(&file, "palette file").unwrap();
    controller
        .palette_mru
        .borrow_mut()
        .push_front("File: palette.txt".into());
    let (window, search) = open(controller).await;
    wait_until("indexed recent file is first", || {
        buttons(&window)
            .first()
            .is_some_and(|(text, _)| text == "File: palette.txt")
    })
    .await;
    let clicked = Rc::new(Cell::new(false));
    let clicked_signal = clicked.clone();
    buttons(&window)[0]
        .1
        .connect_clicked(move |_| clicked_signal.set(true));
    search.emit_activate();
    assert!(
        clicked.get(),
        "Enter must activate the displayed first file"
    );
    wait_until("recent file actually opened", || {
        controller
            .pane_registry
            .borrow()
            .editors
            .values()
            .any(|editor| editor.session_state().active_file.as_ref() == Some(&file))
    })
    .await;
    println!("MACOS_PALETTE_VISIBLE_ORDER_OK");
}
