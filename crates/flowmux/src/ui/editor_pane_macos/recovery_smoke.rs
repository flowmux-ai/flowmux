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
    let original = pane.appearance.borrow().clone();
    for theme in ["one-dark", "github-light"] {
        let options = flowmux_config::options::Options {
            theme: Some(theme.into()),
            ..Default::default()
        };
        let appearance = crate::theme::ResolvedTheme::resolve(&options).editor_appearance(&options);
        let scheme = if appearance.dark { "dark" } else { "light" };
        pane.apply_appearance(appearance);
        wait_js(
            pane,
            &format!("document.documentElement.style.colorScheme === '{scheme}'"),
        )
        .await;
        assert_eq!(
            javascript(
                pane,
                r#"
            (() => {
                const sample = document.createElement('span');
                sample.style.background = 'var(--panel)';
                document.body.append(sample);
                const ctx = document.createElement('canvas').getContext('2d');
                const luminance = color => {
                    ctx.fillStyle = color;
                    ctx.fillRect(0, 0, 1, 1);
                    const rgb = [...ctx.getImageData(0, 0, 1, 1).data].slice(0, 3).map(v => {
                        v /= 255;
                        return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
                    });
                    return rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
                };
                const bg = luminance(getComputedStyle(sample).backgroundColor);
                const readable = ['--warning', '--danger'].every(variable => {
                    sample.style.color = `var(${variable})`;
                    const fg = luminance(getComputedStyle(sample).color);
                    return (Math.max(bg, fg) + 0.05) / (Math.min(bg, fg) + 0.05) >= 4.5;
                });
                sample.remove();
                return String(readable);
            })()
        "#
            )
            .await,
            "true",
            "editor status contrast in {theme}"
        );
    }
    pane.apply_appearance(original);
    println!("MACOS_EDITOR_STATUS_CONTRAST_OK");
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

    // Exercise the native delegate's exhausted-budget UI without private WebKit APIs.
    javascript(pane, "window.beforeRetry = true; 'marked'").await;
    pane.bridge
        .terminations
        .set(super::super::MAX_WEB_PROCESS_RELOADS);
    unsafe {
        WKNavigationDelegate::webViewWebContentProcessDidTerminate(
            &*pane.native._navigation_delegate,
            &pane.native.web_view,
        );
    }
    assert!(pane.bridge.failed.get());
    assert!(!pane.web_widget.is_visible());
    pane.flush_pending_changes().await.unwrap();
    pane.retry_button.emit_clicked();
    wait_js(pane, "typeof window.beforeRetry === 'undefined' && document.querySelector('.view-lines')?.textContent.includes('복구됨')").await;
    assert!(!pane.bridge.failed.get());
    assert!(pane.web_widget.is_visible());
    println!("MACOS_EDITOR_RENDERER_RETRY_OK");
    println!("MACOS_EDITOR_DELETED_RECOVERY_OK");
}
