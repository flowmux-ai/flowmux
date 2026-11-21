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
    for theme in [None, Some("one-dark"), Some("github-light")] {
        let options = flowmux_config::options::Options {
            theme: theme.map(str::to_owned),
            ..Default::default()
        };
        let theme = theme.unwrap_or("default");
        let appearance = crate::theme::ResolvedTheme::resolve(&options).editor_appearance(&options);
        let scheme = if appearance.dark { "dark" } else { "light" };
        let background = serde_json::to_string(&appearance.background).unwrap();
        let foreground = serde_json::to_string(&appearance.foreground).unwrap();
        pane.apply_appearance(appearance);
        wait_js(
            pane,
            // A dark-to-dark switch already matches colorScheme before its queued message arrives.
            &format!("document.documentElement.style.colorScheme === '{scheme}' && document.documentElement.style.getPropertyValue('--ink') === {background} && document.documentElement.style.getPropertyValue('--text') === {foreground}"),
        )
        .await;
        let contrast = javascript(
            pane,
            r#"
            (() => {
                // Canvas readback can be randomized by WebKit tracking protection.
                const luminance = color => {
                    const srgb = color.match(/^color\(srgb ([\d.]+) ([\d.]+) ([\d.]+)\)$/);
                    const legacy = color.match(/^rgb\((\d+), (\d+), (\d+)\)$/);
                    if (!srgb && !legacy) throw new Error(`Unexpected opaque sRGB color: ${color}`);
                    const rgb = (srgb || legacy).slice(1).map(value => {
                        const v = Number(value) / (srgb ? 1 : 255);
                        return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
                    });
                    return rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
                };
                const ratio = (a, b) => (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
                if (ratio(luminance('rgb(0, 0, 0)'), luminance('color(srgb 1 1 1)')) !== 21)
                    throw new Error('Black/white contrast must be 21');
                const colors = ['document-state', 'document-state is-conflict'].map(className => {
                    // Resolve each real status style on a fresh, rendered node inside the modal.
                    const sample = document.createElement('span');
                    sample.className = className;
                    sample.textContent = 'Status contrast';
                    sample.style.background = 'var(--panel)';
                    document.getElementById('recovery-dialog').append(sample);
                    if (!sample.getBoundingClientRect().width) throw new Error('Status sample is not rendered');
                    const style = getComputedStyle(sample);
                    const color = style.color, background = style.backgroundColor;
                    const result = { className, color, background,
                        warning: style.getPropertyValue('--warning'), danger: style.getPropertyValue('--danger'),
                        ratio: ratio(luminance(background), luminance(color)) };
                    sample.remove();
                    return result;
                });
                return JSON.stringify({ colors });
            })()
        "#,
        )
        .await;
        println!("EDITOR_STATUS_CONTRAST theme={theme} {contrast}");
        let contrast: serde_json::Value = serde_json::from_str(&contrast).unwrap();
        assert!(
            contrast["colors"]
                .as_array()
                .unwrap()
                .iter()
                .all(|color| color["ratio"].as_f64().unwrap() >= 4.5),
            "editor status contrast in {theme}: {contrast}"
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
