// SPDX-License-Identifier: GPL-3.0-or-later
use super::save_ack_smoke::{javascript, wait_js};
use super::*;
use flowmux_editor::{LineEnding, RecoverySnapshot, RecoveryStore, TextEncoding};

pub(super) async fn check(pane: &EditorPane, root: &Path) {
    pane.flush_pending_changes().await.unwrap();
    let scope = javascript(pane, "new URLSearchParams(location.search).get('surface')").await;
    let store =
        RecoveryStore::new_scoped(flowmux_config::paths::state_dir().unwrap(), root, &scope)
            .unwrap();
    let identity = std::fs::canonicalize(root)
        .unwrap()
        .join("removed/nested/deleted.txt");
    let recovery = RecoverySnapshot::new(
        store.workspace_id().into(),
        identity.clone(),
        b"\xef\xbb\xbforiginal\r\n",
        2,
        "복구됨\n".into(),
        TextEncoding::Utf8Bom,
        LineEnding::CrLf,
    );
    store.write(&recovery).unwrap();
    pane.open_file(&identity).await.unwrap();
    wait_js(pane, "document.getElementById('recovery-dialog').open").await;
    assert!(!identity.exists());
    javascript(
        pane,
        "document.getElementById('recovery-dialog-restore').click(); 'restore'",
    )
    .await;
    wait_js(
        pane,
        "document.querySelector('.view-lines')?.textContent.includes('복구됨')",
    )
    .await;
    assert!(pane.dirty_document_paths().contains(&identity));
    javascript(
        pane,
        r#"
        document.getElementById('conflict-save-as').click();
        document.getElementById('save-as-path').value = 'recovered.txt';
        document.getElementById('save-as-submit').click();
        'save-as'
    "#,
    )
    .await;
    wait_js(
        pane,
        "document.getElementById('document-state').textContent === 'Saved' && !document.getElementById('save-as-dialog').open",
    )
    .await;
    assert_eq!(
        std::fs::read(root.join("recovered.txt")).unwrap(),
        "\u{feff}복구됨\r\n".as_bytes()
    );
    assert!(!identity.parent().unwrap().exists());
    assert!(store.read(&identity).unwrap().is_none());

    let discarded = identity.with_file_name("discarded.txt");
    store
        .write(&RecoverySnapshot {
            identity_path: discarded.clone(),
            content: String::new(),
            ..recovery
        })
        .unwrap();
    pane.open_file(&discarded).await.unwrap();
    wait_js(pane, "document.getElementById('recovery-dialog').open").await;
    javascript(
        pane,
        "document.getElementById('recovery-dialog-discard').click(); 'discard'",
    )
    .await;
    glib::future_with_timeout(Duration::from_secs(10), async {
        while pane
            .session_state()
            .open_files
            .iter()
            .any(|file| file.path == discarded)
        {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(!discarded.exists());
    assert!(store.read(&discarded).unwrap().is_none());
    println!("MACOS_EDITOR_DELETED_RECOVERY_OK");
}
