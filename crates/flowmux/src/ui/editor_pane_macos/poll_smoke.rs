// SPDX-License-Identifier: GPL-3.0-or-later
use super::save_ack_smoke::{javascript, save_key, wait_js};
use super::*;

pub(super) async fn check(pane: &EditorPane, root: &Path) {
    let first = root.join("poll-first.txt");
    let second = root.join("poll-second.txt");
    std::fs::write(&first, "before").unwrap();
    std::fs::write(&second, "second").unwrap();
    pane.open_file(&first).await.unwrap();
    pane.open_file(&second).await.unwrap();
    pane.open_file(&first).await.unwrap();
    pane.flush_pending_changes().await.unwrap();
    pane.grab_focus();
    wait_js(
        pane,
        "document.querySelector('.view-lines')?.textContent.includes('before')",
    )
    .await;
    std::fs::remove_file(&second).unwrap();
    std::fs::create_dir(&second).unwrap();
    std::fs::write(&first, "EXTERNAL").unwrap();
    wait_js(
        pane,
        "document.querySelector('.view-lines')?.textContent.includes('EXTERNAL')",
    )
    .await;
    javascript(
        pane,
        r#"
        document.querySelector('textarea.inputarea').focus();
        window.flowmuxEditorKeyboard('cursorHome');
        document.execCommand('insertText', false, 'EDIT_');
        'edited'
    "#,
    )
    .await;
    pane.flush_pending_changes().await.unwrap();
    assert!(pane.dirty_document_paths().contains(&first));
    save_key(pane).await;
    wait_js(
        pane,
        "!document.getElementById('document-state').classList.contains('is-dirty')",
    )
    .await;
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "EDIT_EXTERNAL");
    assert!(pane.dirty_document_paths().is_empty());
    println!("MACOS_EDITOR_POLL_PARTIAL_SUCCESS_OK");
}
