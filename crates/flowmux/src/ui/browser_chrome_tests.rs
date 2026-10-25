// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::ui::{browser_pane::BrowserPane, pane_terminal::PaneCallbacks};
use std::rc::Rc;
use std::time::Duration;

fn find(
    root: &impl IsA<gtk::Widget>,
    predicate: &impl Fn(&gtk::Widget) -> bool,
) -> Option<gtk::Widget> {
    let root = root.as_ref();
    if predicate(root) {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find(&widget, predicate) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn button(root: &impl IsA<gtk::Widget>, text: &str) -> gtk::Button {
    find(root, &|w| {
        w.downcast_ref::<gtk::Button>().is_some_and(|b| {
            b.label().as_deref() == Some(text) || b.tooltip_text().as_deref() == Some(text)
        })
    })
    .unwrap_or_else(|| panic!("missing button {text}"))
    .downcast()
    .unwrap()
}

async fn wait(label: &str, ready: impl Fn() -> bool) {
    gtk::glib::future_with_timeout(Duration::from_secs(10), async {
        while !ready() {
            gtk::glib::timeout_future(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("browser layout: {label}"));
}

async fn eval(pane: &BrowserPane, script: &str) -> String {
    let (tx, rx) = tokio::sync::oneshot::channel();
    pane.evaluate_js(script, move |value| {
        let _ = tx.send(value);
    });
    gtk::glib::future_with_timeout(Duration::from_secs(10), rx)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

fn capture(window: &gtk::Window, name: &str) {
    let Some(directory) = std::env::var_os("FLOWMUX_BROWSER_UI_SNAPSHOTS") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(
        &snapshot,
        window.width() as f64,
        window.height() as f64,
    );
    let node = snapshot.to_node().expect("visible browser layout");
    window
        .renderer()
        .unwrap()
        .render_texture(&node, None)
        .save_to_png(directory.join(format!("{name}.png")))
        .unwrap();
}

pub(crate) async fn check() {
    crate::builtin_icons::install();
    #[cfg(target_os = "macos")]
    {
        // The main-thread test harness does not run NSApplication's launch loop.
        let app = objc2_app_kit::NSApplication::sharedApplication(
            objc2::MainThreadMarker::new().unwrap(),
        );
        app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Regular);
        app.finishLaunching();
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
    let fixture = tempfile::tempdir().unwrap();
    let first = fixture.path().join("first.html");
    let second = fixture.path().join("second.html");
    std::fs::write(&first, "<!doctype html><meta charset=utf-8><title>Browser layout</title><style>body{font:16px system-ui;padding:32px}input{padding:12px}</style><h1>Browser workspace</h1><p>페이지를 보면서 작업과 결과를 확인합니다.</p><input id=field aria-label=Message>").unwrap();
    std::fs::write(
        &second,
        "<!doctype html><title>Second page</title><h1>Navigation check</h1>",
    )
    .unwrap();
    let pane = BrowserPane::new(
        flowmux_core::PaneId::new(),
        flowmux_core::SurfaceId::new(),
        Some(&format!("file://{}", first.display())),
        PaneCallbacks::noop_for_test(),
        flowmux_config::options::BrowserEngine::Webkit,
        false,
    );
    let stack = gtk::Stack::new();
    stack.add_named(&pane.root, Some("browser"));
    stack.add_named(&gtk::Label::new(Some("Another tab")), Some("other"));
    let window = gtk::Window::builder()
        .default_width(1200)
        .default_height(720)
        .title("Flowmux browser UI validation")
        .child(&stack)
        .build();
    window.present();
    wait("page ready", || {
        pane.current_title() == "Browser layout" && pane.root.is_mapped()
    })
    .await;
    let toolbar = find(&pane.root, &|w| w.has_css_class("browser-toolbar")).unwrap();
    let address = find(&toolbar, &|w| w.is::<gtk::Entry>())
        .unwrap()
        .downcast::<gtk::Entry>()
        .unwrap();
    #[cfg(target_os = "macos")]
    pane.check_native_chrome_focus(&address);
    // A redirect/state refresh must not replace text being edited in the omnibox.
    address.grab_focus();
    address.set_text("https://example.test/작성중");
    pane.load_uri(&format!("file://{}", second.display()));
    wait("second page", || pane.current_title() == "Second page").await;
    gtk::glib::timeout_future(Duration::from_millis(300)).await;
    assert_eq!(address.text(), "https://example.test/작성중");
    button(&toolbar, "Back").emit_clicked();
    wait("back navigation", || {
        pane.current_title() == "Browser layout"
    })
    .await;
    button(&toolbar, "Forward").emit_clicked();
    wait("forward navigation", || {
        pane.current_title() == "Second page"
    })
    .await;
    button(&toolbar, "Back").emit_clicked();
    wait("back to fixture", || {
        pane.current_title() == "Browser layout"
    })
    .await;
    pane.focus_widget().grab_focus();
    gtk::glib::timeout_future(Duration::from_millis(300)).await;
    pane.set_zoom_level(1.2);
    assert!(find(&toolbar, &|w| w.has_css_class("browser-zoom-badge"))
        .unwrap()
        .is_visible());
    button(&toolbar, "Reset page zoom").emit_clicked();
    assert!(!find(&toolbar, &|w| w.has_css_class("browser-zoom-badge"))
        .unwrap()
        .is_visible());
    let task =
        serde_json::from_value(pane.control.control("start", None).unwrap()["task_id"].clone())
            .unwrap();
    pane.control
        .update_task(
            Some(task),
            flowmux_ipc::protocol::BrowserTaskDetails {
                name: Some("보고서 다운로드 / Account report".into()),
                step: Some("파일 확인".into()),
                waiting: None,
                user_action: Some("결과 파일을 확인하세요".into()),
            },
        )
        .unwrap();
    let file = fixture.path().join("report.txt");
    std::fs::write(&file, "Verified artifact").unwrap();
    pane.control.result(serde_json::json!({"key":"report","title":"report.txt","status":"complete","detail":"Download complete · 파일 다운로드 완료","path":file}));
    let toggle = button(&pane.root, "Results 1")
        .downcast::<gtk::ToggleButton>()
        .unwrap();
    let panel = find(&pane.root, &|w| w.has_css_class("browser-detail")).unwrap();
    assert!(!panel.is_visible());
    toggle.set_active(true);
    wait("wide panel", || {
        panel.width() >= 300 && pane.focus_widget().width() < pane.root.width()
    })
    .await;
    button(&pane.root, "Take control").emit_clicked();
    assert_eq!(pane.control.status()["state"], "paused");
    button(&pane.root, "Continue").emit_clicked();
    assert_eq!(pane.control.status()["state"], "running");
    // macOS constrains the initial window to the hosted runner's display width.
    let wide_width = window.width();
    assert!(
        wide_width >= 900,
        "display must support the wide task panel"
    );
    for preset in ["github-dark", "github-light"] {
        let options = flowmux_config::options::Options {
            theme: Some(preset.into()),
            ..Default::default()
        };
        let theme = crate::theme::ResolvedTheme::resolve(&options);
        let provider = gtk::CssProvider::new();
        let errors = Rc::new(Cell::new(false));
        let failed = errors.clone();
        provider.connect_parsing_error(move |_, _, error| {
            eprintln!("browser CSS: {error}");
            failed.set(true);
        });
        provider.load_from_string(&theme.css_for_options(&options));
        assert!(!errors.get());
        gtk::style_context_add_provider_for_display(
            &gtk::prelude::WidgetExt::display(&window),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        for width in [wide_width, 720, 480] {
            window.set_default_size(width, 720);
            wait(&format!("resize to {width}"), || window.width() == width).await;
            gtk::glib::timeout_future(Duration::from_millis(150)).await;
            let body = panel.parent().unwrap().downcast::<gtk::Box>().unwrap();
            assert_eq!(
                body.orientation(),
                if width >= 900 {
                    gtk::Orientation::Horizontal
                } else {
                    gtk::Orientation::Vertical
                }
            );
            assert!(toolbar.width() <= width && address.width() > 80);
            for name in ["Back", "Forward", "Reload page"] {
                assert!(
                    button(&toolbar, name).is_mapped(),
                    "{name} must remain in the toolbar at width {width}"
                );
            }
            if width >= 900 {
                assert!(panel.width() <= 360, "panel too wide: {}", panel.width());
            }
            assert!(pane.focus_widget().height() > 250);
            capture(&window, &format!("{preset}-{width}"));
        }
        for font_size in [19.5, 26.0] {
            provider.load_from_string(&format!(
                "{}\n.flowmux-browser {{ font-size: {font_size}px; }}",
                theme.css_for_options(&options)
            ));
            gtk::glib::timeout_future(Duration::from_millis(200)).await;
            assert_eq!(
                window.width(),
                480,
                "enlarged text must not force the pane wider"
            );
            assert!(address.width() > 80);
            capture(&window, &format!("{preset}-480-font-{font_size}"));
        }
        gtk::style_context_remove_provider_for_display(
            &gtk::prelude::WidgetExt::display(&window),
            &provider,
        );
    }
    let more = find(&toolbar, &|w| {
        w.downcast_ref::<gtk::MenuButton>().is_some_and(|b| {
            b.tooltip_text()
                .is_some_and(|t| t.starts_with("Browser tools"))
        })
    })
    .unwrap()
    .downcast::<gtk::MenuButton>()
    .unwrap();
    more.popup();
    wait("tools menu", || more.popover().unwrap().is_mapped()).await;
    let downloads = find(&toolbar, &|w| {
        w.downcast_ref::<gtk::MenuButton>()
            .is_some_and(|b| b.tooltip_text().as_deref() == Some("Downloads"))
    })
    .unwrap()
    .downcast::<gtk::MenuButton>()
    .unwrap();
    downloads.popup();
    wait("overflow downloads", || {
        downloads.popover().unwrap().is_mapped()
    })
    .await;
    assert!(more.popover().unwrap().is_visible());
    downloads.popdown();
    button(&toolbar, "Find in page").emit_clicked();
    let search = find(&pane.root, &|w| w.is::<gtk::SearchEntry>()).unwrap();
    assert!(search.is_visible());
    button(&pane.root, "Close find").emit_clicked();
    assert!(!search.is_visible());
    stack.set_visible_child_name("other");
    wait("hidden browser tab", || !pane.root.is_mapped()).await;
    stack.set_visible_child_name("browser");
    wait("restored browser tab", || pane.root.is_mapped()).await;
    assert_eq!(pane.control.status()["task_id"], task.to_string());
    assert!(toolbar.child_focus(gtk::DirectionType::TabForward));
    button(&pane.root, "Close task details").emit_clicked();
    assert!(!panel.is_visible());
    assert_eq!(
        pane.control.status()["results"].as_array().unwrap().len(),
        1
    );
    pane.control.result(
        serde_json::json!({"key":"second","title":"Input checked","status":"outcome_confirmed"}),
    );
    assert!(!panel.is_visible(), "new results must not resize the page");
    // GTK allocation and the native WebView frame settle on separate turns.
    for _ in 0..400 {
        gtk::glib::timeout_future(Duration::from_millis(25)).await;
        if eval(
            &pane,
            &format!(
                "String(innerWidth === {} && innerHeight === {})",
                pane.focus_widget().width(),
                pane.focus_widget().height()
            ),
        )
        .await
            == "true"
        {
            break;
        }
    }
    assert_eq!(
        eval(
            &pane,
            &format!(
                "String(innerWidth === {} && innerHeight === {})",
                pane.focus_widget().width(),
                pane.focus_widget().height()
            )
        )
        .await,
        "true",
        "GTK {}x{}, DOM {}",
        pane.focus_widget().width(), pane.focus_widget().height(),
        eval(&pane, "JSON.stringify({width:innerWidth,height:innerHeight,hidden:document.hidden,focus:document.hasFocus(),scale:devicePixelRatio})").await
    );
    let target: flowmux_browser::input::PointerTarget = serde_json::from_str(&eval(&pane,
        "JSON.stringify((()=>{const r=document.querySelector('#field').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2,width:innerWidth,height:innerHeight}})())").await).unwrap();
    pane.pointer(flowmux_browser::input::PointerAction::Click, &target)
        .unwrap();
    for _ in 0..40 {
        if eval(&pane, "document.activeElement.id").await == "field" {
            break;
        }
        gtk::glib::timeout_future(Duration::from_millis(25)).await;
    }
    assert_eq!(eval(&pane, "document.activeElement.id").await, "field");
    pane.insert_text("한글 입력").unwrap();
    gtk::glib::timeout_future(Duration::from_millis(50)).await;
    assert_eq!(
        eval(&pane, "document.querySelector('#field').value").await,
        "한글 입력"
    );
    button(&pane.root, "Stop").emit_clicked();
    assert_eq!(pane.control.status()["state"], "stopped");
    toggle.set_active(true);
    button(&pane.root, "Clear task display").emit_clicked();
    assert!(!panel.is_visible());
    assert!(pane.control.status()["task_id"].is_null());
    pane.prepare_for_close();
    window.destroy();
    println!("BROWSER_CHROME_LAYOUT_OK");
}

#[cfg(all(test, target_os = "linux"))]
#[gtk::test]
async fn browser_chrome_layout_and_controls() {
    check().await;
}
