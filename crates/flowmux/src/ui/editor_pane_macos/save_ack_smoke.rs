// SPDX-License-Identifier: GPL-3.0-or-later
//! Delay actual host save acknowledgements in the live WKWebView.
use super::*;

pub(super) async fn javascript(pane: &EditorPane, source: &str) -> String {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Rc::new(RefCell::new(Some(tx)));
    let block = block2::RcBlock::new(
        move |value: *mut objc2::runtime::AnyObject, error: *mut NSError| {
            let result = if !error.is_null() {
                Err(unsafe { (&*error).localizedDescription().to_string() })
            } else if value.is_null() {
                Ok(String::new())
            } else {
                Ok(unsafe { (&*value).downcast_ref::<NSString>() }
                    .map(ToString::to_string)
                    .unwrap_or_default())
            };
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(result);
            }
        },
    );
    unsafe {
        pane.native
            .web_view
            .evaluateJavaScript_completionHandler(&NSString::from_str(source), Some(&block));
    }
    glib::future_with_timeout(Duration::from_secs(10), rx)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

pub(super) async fn wait_js(pane: &EditorPane, predicate: &str) {
    glib::future_with_timeout(Duration::from_secs(15), async {
        while javascript(pane, &format!("String({predicate})")).await != "true" {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("editor predicate timed out: {predicate}"));
}

pub(super) async fn save_key(pane: &EditorPane) {
    javascript(
        pane,
        r#"
            document.querySelector('textarea.inputarea').focus();
            document.activeElement.dispatchEvent(new KeyboardEvent('keydown', {
                key: 's', code: 'KeyS', keyCode: 83, which: 83, metaKey: true,
                bubbles: true, cancelable: true
            }));
            'saved'
        "#,
    )
    .await;
}

pub(super) async fn check(pane: &EditorPane, root: &Path) {
    for save_as in [false, true] {
        let name = if save_as { "save-as.txt" } else { "save.txt" };
        let file = root.join(name);
        std::fs::write(&file, "original").unwrap();
        pane.open_file(&file).await.unwrap();
        pane.flush_pending_changes().await.unwrap();
        pane.grab_focus();
        wait_js(
            pane,
            "document.querySelector('textarea.inputarea') !== null",
        )
        .await;
        javascript(
            pane,
            r#"
            document.querySelector('textarea.inputarea').focus();
            window.flowmuxEditorKeyboard('cursorHome');
            document.execCommand('insertText', false, 'EARLIER_');
            window.auditAck = null;
            window.auditReceive = window.flowmuxEditorHost.receive;
            window.flowmuxEditorHost.receive = message => {
                if (message.type === 'save_completed' || message.type === 'save_as_completed') {
                    window.auditAck = message;
                } else { window.auditReceive(message); }
            };
            'armed'
        "#,
        )
        .await;
        pane.flush_pending_changes().await.unwrap();
        assert!(
            pane.dirty_document_paths().contains(&file),
            "initial edit did not reach host"
        );
        let saved_path = if save_as {
            javascript(
                pane,
                r#"
                document.getElementById('conflict-save-as').click();
                document.getElementById('save-as-path').value = 'renamed.txt';
                document.getElementById('save-as-submit').click();
                'requested'
            "#,
            )
            .await;
            std::fs::canonicalize(root).unwrap().join("renamed.txt")
        } else {
            save_key(pane).await;
            file.clone()
        };
        wait_js(pane, "window.auditAck !== null").await;
        if save_as {
            // Cancel dismisses the pending dialog without cancelling the host write.
            javascript(
                pane,
                "document.getElementById('save-as-cancel').click(); 'closed'",
            )
            .await;
        }
        let saved = std::fs::read_to_string(&saved_path).unwrap();
        assert!(saved.contains("EARLIER_"), "initial edit missing: {saved}");
        // Input and acknowledgement share one JS task, before the 150ms change timer.
        let state = javascript(
            pane,
            r#"
            document.querySelector('textarea.inputarea').focus();
            window.flowmuxEditorKeyboard('cursorHome');
            document.execCommand('insertText', false, 'NEWER_');
            window.flowmuxEditorHost.receive = window.auditReceive;
            window.auditReceive(window.auditAck);
            document.getElementById('document-state').textContent
        "#,
        )
        .await;
        assert_eq!(state, "Unsaved", "late save ack cleared a new edit");
        pane.flush_pending_changes().await.unwrap();
        assert!(
            pane.dirty_document_paths().contains(&saved_path),
            "save_as={save_as}, dirty={:?}, active={:?}, state={}",
            pane.dirty_document_paths(),
            pane.session_state().active_file,
            javascript(
                pane,
                "document.getElementById('document-state').textContent"
            )
            .await
        );
        assert_eq!(std::fs::read_to_string(&saved_path).unwrap(), saved);
        assert_eq!(pane.session_state().active_file.as_ref(), Some(&saved_path));
        // Use Monaco's normal Save keybinding, so a false clean flag cannot be hidden
        // by a direct host save call.
        save_key(pane).await;
        wait_js(
            pane,
            "!document.getElementById('document-state').classList.contains('is-dirty')",
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(&saved_path).unwrap(),
            format!("NEWER_{saved}")
        );
        println!("MACOS_EDITOR_LATE_SAVE_ACK_OK save_as={save_as}");
    }
}
