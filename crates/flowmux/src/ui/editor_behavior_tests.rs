// SPDX-License-Identifier: GPL-3.0-or-later
//! Exercise the shipped Monaco bundle through the real WebKit/host bridge.

use super::*;
use gtk::glib;
use std::time::Instant;

struct EditorPage {
    pane: EditorPane,
    window: gtk::Window,
}

impl EditorPage {
    async fn eval(&self, script: &str) -> String {
        let result = glib::future_with_timeout(
            Duration::from_secs(15),
            self.pane
                .web_view
                .evaluate_javascript_future(script, None, None),
        )
        .await
        .expect("editor JavaScript timed out")
        .unwrap_or_else(|error| panic!("{error}: {script}"));
        result.to_str().to_string()
    }

    async fn wait(&self, predicate: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.eval(predicate).await != "true" {
            assert!(
                Instant::now() < deadline,
                "editor did not reach: {predicate}"
            );
            glib::timeout_future(Duration::from_millis(25)).await;
        }
    }

    async fn insert(&self, text: &str) {
        self.eval("document.querySelector('#editor textarea').focus(); window.flowmuxEditorKeyboard('cursorHome')")
            .await;
        self.pane
            .web_view
            .execute_editing_command_with_argument("InsertText", text);
        self.wait("document.querySelector('#document-state').textContent === 'Unsaved'")
            .await;
        self.pane.flush_pending_changes().await.unwrap();
    }
}

impl Drop for EditorPage {
    fn drop(&mut self) {
        self.pane.prepare_for_close();
        self.window.destroy();
    }
}

#[gtk::test]
async fn shipped_editor_edits_saves_detects_conflicts_and_recovers_web_process() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("문서🙂.txt");
    fs::write(&path, "original\n").unwrap();
    let options = flowmux_config::options::Options::default();
    let appearance = crate::theme::ResolvedTheme::resolve(&options).editor_appearance(&options);
    let pane = EditorPane::new(
        PaneId::new(),
        SurfaceId::new(),
        dir.path().to_path_buf(),
        EditorSessionState::default(),
        appearance,
    )
    .unwrap();
    let window = gtk::Window::builder()
        .default_width(900)
        .default_height(600)
        .child(&pane.root)
        .build();
    window.present();
    let page = EditorPage { pane, window };
    // Queue the open before editor_ready: the real handshake must deliver it.
    page.pane.open_file(&path).unwrap();
    page.wait("typeof window.flowmuxEditorHost === 'object' && document.querySelector('#editor .view-lines')?.textContent.includes('original') === true").await;
    assert!(page.pane.bridge.ready.get());
    assert_eq!(page.pane.session_state().active_file.as_ref(), Some(&path));

    page.insert("한글🙂 ").await;
    assert_eq!(page.pane.dirty_document_paths(), vec![path.clone()]);
    assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
    page.eval("document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 's', code: 'KeyS', keyCode: 83, ctrlKey: true, bubbles: true, cancelable: true }))").await;
    page.wait("document.querySelector('#document-state').hidden")
        .await;
    assert_eq!(fs::read_to_string(&path).unwrap(), "한글🙂 original\n");
    assert!(page.pane.dirty_document_paths().is_empty());

    page.insert("unsaved ").await;
    fs::write(&path, "external update\n").unwrap();
    // Native file monitoring/polling must update the actual conflict UI.
    page.wait("!document.querySelector('#conflict-banner').hidden")
        .await;
    assert!(page.pane.save_all_dirty().is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "external update\n");
    page.eval("document.querySelector('#conflict-reload').click()")
        .await;
    page.wait("document.querySelector('#conflict-banner').hidden && document.querySelector('#editor .view-lines').textContent.includes('external')").await;
    assert!(page.pane.dirty_document_paths().is_empty());

    page.insert("recovered ").await;
    page.eval("window.beforeCrash = true").await;
    page.pane.web_view.terminate_web_process();
    page.wait("typeof window.beforeCrash === 'undefined' && typeof window.flowmuxEditorHost === 'object' && document.querySelector('#editor .view-lines')?.textContent.includes('recovered') === true").await;
    page.pane.flush_pending_changes().await.unwrap();
    page.pane.save_all_dirty().unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "recovered external update\n"
    );

    page.pane.show_workspace_search();
    page.wait("document.querySelector('#search-dialog').open")
        .await;
    page.eval("const q = document.querySelector('#search-query'); q.value = 'recovered'; q.dispatchEvent(new Event('input', { bubbles: true }))").await;
    page.wait("document.querySelector('#search-results').textContent.includes('문서🙂.txt') && document.querySelector('#search-results').textContent.includes('recovered')").await;
    // External navigation must not replace the privileged editor document.
    let uri = page.pane.web_view.uri().unwrap();
    page.pane.web_view.load_uri("file:///etc/passwd");
    glib::timeout_future(Duration::from_millis(200)).await;
    assert_eq!(page.pane.web_view.uri().as_ref(), Some(&uri));
    page.wait("document.querySelector('#search-dialog').open")
        .await;
}
