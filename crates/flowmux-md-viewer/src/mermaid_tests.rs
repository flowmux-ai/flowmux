// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use std::time::{Duration, Instant};

async fn eval(view: &webkit6::WebView, script: &str) -> String {
    glib::future_with_timeout(
        Duration::from_secs(15),
        view.evaluate_javascript_future(script, Some(MARKDOWN_SCRIPT_WORLD), None),
    )
    .await
    .expect("JavaScript timed out")
    .unwrap_or_else(|error| panic!("{error}: {script}"))
    .to_str()
    .to_string()
}

async fn wait(view: &webkit6::WebView, predicate: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while eval(view, predicate).await != "true" {
        assert!(
            Instant::now() < deadline,
            "Markdown did not reach: {predicate}"
        );
        glib::timeout_future(Duration::from_millis(25)).await;
    }
}

#[gtk::test]
async fn diagrams_errors_security_live_reload_and_zoom() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mermaid.md");
    let markdown = include_str!("../tests/fixtures/mermaid.md");
    std::fs::write(&path, markdown).unwrap();
    let options = RenderOptions::default();
    let view = markdown_web_view(&options);
    let window = gtk::Window::builder()
        .default_width(900)
        .default_height(700)
        .child(&view)
        .build();
    window.present();
    let state = Rc::new(RefCell::new(ViewerState {
        path: path.clone(),
        options,
        web_view: view.clone(),
        zoom_label: gtk::Label::new(None),
        navigation: NavigationState::new(),
        last_signature: file_signature(&path),
    }));
    let monitor = install_live_reload(&state).unwrap();
    state.borrow_mut().render();
    wait(&view, "document.querySelectorAll('.mermaid-chart > svg').length === 4 && document.querySelectorAll('.mermaid-error').length === 1").await;
    assert_eq!(
        eval(
            &view,
            "document.querySelectorAll('code.language-mermaid').length"
        )
        .await,
        "1"
    );
    assert_eq!(
        eval(
            &view,
            "document.querySelector('code.language-rust').textContent.trim()"
        )
        .await,
        "let value = \"<tag>\";"
    );
    assert_eq!(
        eval(
            &view,
            "document.querySelector('.mermaid-chart').textContent.includes('시작')"
        )
        .await,
        "true"
    );
    assert_eq!(eval(&view, "document.querySelectorAll('.mermaid-chart > svg').length === new Set(Array.from(document.querySelectorAll('.mermaid-chart > svg'), s => s.id)).size").await, "true");
    assert_eq!(
        eval(
            &view,
            "document.querySelector('.mermaid-chart svg').getBoundingClientRect().height > 20"
        )
        .await,
        "true"
    );

    // Page markup, event handlers, javascript: links and diagram directives cannot run code.
    assert_eq!(
        eval(&view, "document.documentElement.dataset.executed || 'safe'").await,
        "safe"
    );
    eval(&view, "document.querySelector('#script-link').click(); document.querySelector('#event-button').click(); 'clicked'").await;
    glib::timeout_future(Duration::from_millis(100)).await;
    assert_eq!(
        eval(&view, "document.documentElement.dataset.executed || 'safe'").await,
        "safe"
    );
    assert_eq!(eval(&view, "document.querySelectorAll('.mermaid-chart script, .mermaid-chart [onerror], .mermaid-chart [onclick]').length").await, "0");

    std::fs::write(
        &path,
        "# Updated\n\n```mermaid\nflowchart LR\nA[갱신 완료] --> B[Done]\n```\n",
    )
    .unwrap();
    wait(&view, "document.querySelector('h1')?.textContent.includes('Updated') && document.querySelector('.mermaid-chart > svg')?.textContent.includes('갱신 완료')").await;
    eval(
        &view,
        "window.originalDiagram = document.querySelector('.mermaid-chart > svg'); 'saved'",
    )
    .await;
    let zoom_loads = Rc::new(Cell::new(0));
    let loads = zoom_loads.clone();
    let handler = view.connect_load_changed(move |_, event| {
        if event == webkit6::LoadEvent::Started {
            loads.set(loads.get() + 1);
        }
    });
    state.borrow_mut().set_zoom(1.25);
    glib::timeout_future(Duration::from_millis(100)).await;
    assert_eq!(view.zoom_level(), 1.25);
    assert_eq!(state.borrow().zoom_label.text(), "125%");
    assert_eq!(zoom_loads.get(), 0, "zoom must not reload the document");
    assert_eq!(
        eval(
            &view,
            "window.originalDiagram === document.querySelector('.mermaid-chart > svg')"
        )
        .await,
        "true"
    );
    view.disconnect(handler);

    std::fs::write(&path, "# Plain Markdown\n\nNo chart.").unwrap();
    wait(
        &view,
        "document.querySelector('h1')?.textContent.includes('Plain Markdown')",
    )
    .await;
    assert_eq!(
        eval(
            &view,
            "document.querySelectorAll('svg, .mermaid-error').length"
        )
        .await,
        "0"
    );
    monitor.cancel();
    window.close();
}
