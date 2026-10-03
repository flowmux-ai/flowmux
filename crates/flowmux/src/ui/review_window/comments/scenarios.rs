// SPDX-License-Identifier: GPL-3.0-or-later
//! Live GTK scenarios run by the native main-thread harness.
use super::*;
use crate::bridge::{Bridge, GtkCommand};

async fn settled(view: &ReviewWindow) {
    glib::future_with_timeout(std::time::Duration::from_secs(20), async {
        while !view.root_widget.is_mapped()
            || !view.refresh.is_sensitive()
            || view.patch.borrow().is_none()
            || view.comments.busy.get()
            || !view.comments.ready.get()
        {
            glib::timeout_future(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

fn button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        if button.label().as_deref() == Some(label) {
            return Some(button.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Some(button) = button(&widget, label) {
            return Some(button);
        }
        child = widget.next_sibling();
    }
    None
}

fn select_line(view: &ReviewWindow, line: u32) {
    let patch = view.patch.borrow();
    let row = view
        .display_lines
        .borrow()
        .iter()
        .position(|raw| raw.is_some_and(|i| patch.as_ref().unwrap().lines[i].new == Some(line)))
        .unwrap();
    let buffer = view.diff.buffer();
    buffer.place_cursor(&buffer.iter_at_line(row as i32).unwrap());
}

fn assert_saved_at(view: &ReviewWindow, id: &str, line: u32) {
    let draft = view.comments.draft.borrow();
    assert_eq!(draft.notes.len(), 1);
    assert_eq!(draft.notes[0].id, id);
    assert_eq!(draft.notes[0].new_lines, Some((line, line)));
    assert!(!draft.notes[0].needs_reattach);
    assert_eq!(
        DraftStore::default_store()
            .unwrap()
            .load(&view.root)
            .unwrap(),
        *draft
    );
}

fn open(
    parent: &adw::ApplicationWindow,
    root: &std::path::Path,
) -> (adw::ApplicationWindow, Rc<ReviewWindow>) {
    let window = adw::ApplicationWindow::builder()
        .transient_for(parent)
        .default_width(950)
        .default_height(700)
        .build();
    let host = gtk::Stack::new();
    host.add_child(&gtk::Label::new(Some("Terminal placeholder")));
    window.set_content(Some(&host));
    let view = ReviewWindow::new(&window, &host, flowmux_core::PaneId::new(), root.into());
    view.present();
    (window, view)
}

pub(super) async fn run(parent: &adw::ApplicationWindow) {
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .unwrap()
        .status
        .success());
    let file = dir.path().join("anchor.txt");
    let original = "before\n    target\nafter\n";
    std::fs::write(&file, original).unwrap();
    let (window, view) = open(parent, dir.path());
    settled(&view).await;
    select_line(&view, 2);
    view.begin_comment(false);
    view.comments.writer.buffer().set_text("시나리오 코멘트");
    view.comments.save.emit_clicked();
    settled(&view).await;
    let id = view.comments.draft.borrow().notes[0].id.clone();
    assert_saved_at(&view, &id, 2);

    // Actual Refresh button relocates both the card and persisted draft.
    std::fs::write(&file, "prefix\nbefore\n\ttarget\nchanged after\n").unwrap();
    view.refresh.emit_clicked();
    settled(&view).await;
    assert_saved_at(&view, &id, 3);
    let label = view.inline_widgets.borrow()[0]
        .first_child()
        .unwrap()
        .downcast::<gtk::Label>()
        .unwrap();
    assert_eq!(label.text(), "new lines 3–3");
    println!("REVIEW_SCENARIO_REFRESH_RELOCATES_CARD_AND_STORAGE_OK");

    // Hiding/reopening the same pane must reload external edits.
    view.hide();
    std::fs::write(
        &file,
        "another prefix\nprefix\nbefore\n\ttarget\nchanged after\n",
    )
    .unwrap();
    view.present();
    settled(&view).await;
    assert_saved_at(&view, &id, 4);
    window.destroy();
    drop(view);

    // Reconstructing a view uses persisted code/context, not the old widget.
    let current = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("reopen prefix\n{current}")).unwrap();
    let (window, view) = open(parent, dir.path());
    settled(&view).await;
    assert_saved_at(&view, &id, 5);
    println!("REVIEW_SCENARIO_REOPEN_RESTORES_AND_RELOCATES_OK");

    let (bridge, receiver) = Bridge::new();
    view.connect_targets(bridge);
    let target = ReviewTarget {
        surface: flowmux_core::SurfaceId::new(),
        name: "codex".into(),
        pid: Some(123),
        session: Some("scenario".into()),
        label: "test receiver".into(),
    };
    let current = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("send prefix\n{current}")).unwrap();
    view.send_review(Some(target.clone()));
    settled(&view).await;
    assert_saved_at(&view, &id, 6);
    let mut sent = false;
    while let Ok(command) = receiver.try_recv() {
        if let GtkCommand::FocusReviewTarget {
            target: delivered,
            prompt,
            ..
        } = command
        {
            assert_eq!(delivered, target);
            assert!(prompt.contains("new lines 6–6"));
            assert!(prompt.contains("시나리오 코멘트") && prompt.contains("> +\ttarget"));
            sent = true;
        }
    }
    assert!(sent, "send must prepare a current anchor before dispatch");
    println!("REVIEW_SCENARIO_SEND_REFRESHES_BEFORE_DISPATCH_OK");

    // Duplicate the whole neighborhood immediately before sending; do not
    // refresh manually. Both clipboard and agent dispatch must be blocked.
    let current = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, current.repeat(2)).unwrap();
    let clipboard = view.parent.clipboard().read_text_future().await.unwrap();
    for destination in [None, Some(target)] {
        view.send_review(destination);
        settled(&view).await;
        assert_eq!(view.comments.draft.borrow().notes.len(), 1);
        assert!(view.comments.draft.borrow().notes[0].needs_reattach);
        assert!(view.status.text().contains("Reattach"));
        assert_eq!(
            view.parent.clipboard().read_text_future().await.unwrap(),
            clipboard
        );
        while let Ok(command) = receiver.try_recv() {
            assert!(!matches!(command, GtkCommand::FocusReviewTarget { .. }));
        }
        assert!(
            button(view.diff.upcast_ref(), "Reattach").is_some(),
            "blocked feedback must expose recovery in the visible card"
        );
    }
    println!("REVIEW_SCENARIO_AMBIGUOUS_SEND_PRESERVES_AND_BLOCKS_OK");

    // Explicitly attaching one of the duplicates resolves ambiguity without
    // changing the comment ID or feedback text.
    view.refresh.emit_clicked();
    settled(&view).await;
    select_line(&view, 6);
    button(view.diff.upcast_ref(), "Reattach")
        .unwrap()
        .emit_clicked();
    assert_eq!(text(&view.comments.writer), "시나리오 코멘트");
    view.comments.save.emit_clicked();
    settled(&view).await;
    assert_saved_at(&view, &id, 6);
    view.send_review(None);
    settled(&view).await;
    assert_eq!(view.status.text(), "Feedback copied");
    assert!(view
        .parent
        .clipboard()
        .read_text_future()
        .await
        .unwrap()
        .unwrap()
        .contains("new lines 6–6"));
    window.destroy();
    println!("REVIEW_SCENARIO_REATTACH_PRESERVES_ID_AND_UNBLOCKS_OK");
}
