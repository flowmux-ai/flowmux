// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use gtk::glib::translate::ToGlibPtr;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSWindow};
use objc2_foundation::{NSPoint, NSString};

unsafe extern "C" {
    fn gdk_macos_surface_get_native_window(
        surface: *mut gtk::gdk::ffi::GdkSurface,
    ) -> *mut std::ffi::c_void;
}

pub(super) async fn check(
    controller: &WindowController,
    workspace: WorkspaceId,
    terminal_a: PaneId,
    editor_a: PaneId,
    root: &std::path::Path,
) {
    let terminal_b = controller
        .split_pane(terminal_a, SplitDirection::Horizontal)
        .await
        .unwrap();
    let editor_b = controller
        .split_pane(editor_a, SplitDirection::Horizontal)
        .await
        .unwrap();
    let (_, surface) = controller
        .store
        .add_editor_surface_to_pane(editor_b, root.into())
        .await
        .unwrap();
    controller
        .attach_or_rerender_surface(workspace, editor_b, surface, None)
        .await
        .unwrap();
    let editors = [editor_a, editor_b].map(|pane| {
        controller
            .pane_registry
            .borrow()
            .active_editor(pane)
            .unwrap()
            .clone()
    });
    let terminals = [terminal_a, terminal_b].map(|pane| {
        controller
            .pane_registry
            .borrow()
            .active_terminal(pane)
            .unwrap()
            .clone()
    });
    wait_until("four panes mapped", || {
        editors.iter().all(|e| e.focus_widget().is_mapped())
            && terminals.iter().all(|t| t.widget.is_mapped())
    })
    .await;
    glib::timeout_future(Duration::from_millis(100)).await;
    for _ in 0..20 {
        for pane in [
            editor_a, terminal_a, terminal_b, editor_b, editor_a, editor_b, terminal_b, terminal_a,
        ] {
            controller.focus_pane(pane);
            for (id, editor) in [editor_a, editor_b].into_iter().zip(&editors) {
                assert_eq!(editor.has_native_focus(), id == pane);
                assert_eq!(editor.focus_widget().is_focus(), id == pane);
            }
            for (id, terminal) in [terminal_a, terminal_b].into_iter().zip(&terminals) {
                assert_eq!(terminal.widget.is_focus(), id == pane);
            }
        }
    }
    println!("MACOS_FOUR_PANE_IMMEDIATE_FOCUS_OK transitions=160");

    let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
    app.activate();
    let surface = controller.window.surface().unwrap();
    let window = unsafe {
        &*gdk_macos_surface_get_native_window(surface.to_glib_none().0).cast::<NSWindow>()
    };
    window.makeKeyAndOrderFront(None);
    for (index, editor) in editors.iter().enumerate() {
        let file = root.join(format!("keyboard-editor-{index}.txt"));
        std::fs::write(&file, "").unwrap();
        editor.open_file(&file).await.unwrap();
        editor.flush_pending_changes().await.unwrap();
    }
    for (index, terminal) in terminals.iter().enumerate() {
        terminal
            .write_input(
                format!("cat > '{}/keyboard-terminal-{index}.txt'\n", root.display()).as_bytes(),
            )
            .unwrap();
    }
    glib::timeout_future(Duration::from_millis(200)).await;
    controller.focus_pane(terminal_b);
    for pane in [editor_a, terminal_a, editor_b, terminal_b] {
        if pane == editor_a {
            editors[0].focus_native_only_for_test();
        } else if pane == editor_b {
            editors[1].focus_native_only_for_test();
        } else {
            controller.focus_pane(pane);
        }
        for (text, code) in [("1", 18), (" ", 49), ("2", 19), ("\r", 36)] {
            key(&app, window, text, code, false).await;
        }
        let is_editor = [editor_a, editor_b].contains(&pane);
        if is_editor {
            key(&app, window, "\t", 48, false).await;
        }
        for (text, code) in [("3", 20), ("\u{7f}", 51), ("4", 21)] {
            key(&app, window, text, code, false).await;
        }
        if is_editor {
            key(&app, window, "\t", 48, true).await;
        }
        key(&app, window, "\r", 36, false).await;
    }
    for editor in &editors {
        editor.flush_pending_changes().await.unwrap();
        editor.save_all_dirty().await.unwrap();
    }
    for kind in ["editor", "terminal"] {
        for index in 0..2 {
            let file = root.join(format!("keyboard-{kind}-{index}.txt"));
            assert_eq!(
                std::fs::read_to_string(file).unwrap(),
                "1 2\n4\n",
                "{kind} {index}: editing keys were lost or duplicated"
            );
        }
    }
    println!("MACOS_FOUR_PANE_EDITING_KEYS_OK");
}

async fn key(app: &NSApplication, window: &NSWindow, text: &str, code: u16, shift: bool) {
    let text = NSString::from_str(text);
    let flags = if shift {
        NSEventModifierFlags::Shift
    } else {
        NSEventModifierFlags::empty()
    };
    for event_type in [NSEventType::KeyDown, NSEventType::KeyUp] {
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            event_type, NSPoint::new(0.0, 0.0), flags,
            glib::monotonic_time() as f64 / 1_000_000.0, window.windowNumber(),
            None, &text, &text, false, code,
        ).unwrap();
        app.postEvent_atStart(&event, false);
    }
    glib::timeout_future(Duration::from_millis(5)).await;
}
