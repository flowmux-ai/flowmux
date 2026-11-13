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
