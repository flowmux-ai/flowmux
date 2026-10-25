// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

impl BrowserPane {
    pub(crate) async fn check_native_inspector(app: &gtk::Application) {
        let pane = Self::new(
            PaneId::new(),
            SurfaceId::new(),
            Some("about:blank"),
            PaneCallbacks::noop_for_test(),
            BrowserEngine::Webkit,
            false,
        );
        let window = gtk::Window::builder()
            .application(app)
            .default_width(900)
            .default_height(600)
            .child(&pane.root)
            .build();
        window.set_title(Some("Flowmux Inspector regression"));
        window.present();
        wait(|| pane.native.web_view.as_super().window().is_some()).await;
        assert!(
            unsafe { pane.native.web_view.isInspectable() },
            "browser must be inspectable in release builds"
        );
        unsafe {
            pane.native.web_view.loadHTMLString_baseURL(&NSString::from_str("<!doctype html><title>Inspector fixture</title><body style=\"background:#16394f;color:white;font:24px system-ui\"><h1>Browser remains visible</h1><input id=field value=Hello>"), None);
        }
        wait(|| pane.current_title() == "Inspector fixture").await;
        let button = button(pane.root.upcast_ref()).expect("Inspector toolbar button");
        let inspector: Retained<AnyObject> =
            unsafe { msg_send![&*pane.native.web_view, _inspector] };
        for _ in 0..2 {
            button.emit_clicked();
            wait(|| unsafe {
                let visible: Bool = msg_send![&*inspector, isVisible];
                visible.as_bool()
            })
            .await;
            let frontend: Retained<WKWebView> =
                unsafe { msg_send![&*inspector, extensionHostWebView] };
            assert_ne!(
                frontend.as_super().window(),
                pane.native.web_view.as_super().window(),
                "Inspector must not attach to the GTK window and resize its native browser view"
            );
            wait(|| !unsafe { frontend.isLoading() }).await;
            assert!(unsafe { frontend.URL() }
                .unwrap()
                .absoluteString()
                .unwrap()
                .to_string()
                .contains("Main.html"));
            wait(|| {
                pane.native.web_view.as_super().frame().size.height
                    == f64::from(pane.web_view.height())
            })
            .await;
            for _ in 0..5 {
                glib::timeout_future(Duration::from_millis(50)).await;
                let frame = pane.native.web_view.as_super().frame();
                assert_eq!(frame.size.width, f64::from(pane.web_view.width()));
                assert_eq!(frame.size.height, f64::from(pane.web_view.height()));
                assert!(!pane.native.web_view.as_super().isHidden());
            }
            // Web Inspector also exposes docking buttons after opening.
            unsafe {
                let _: () = msg_send![&*inspector, attach];
            }
            wait(|| frontend.as_super().window() != pane.native.web_view.as_super().window()).await;
            assert!(frontend.as_super().window().unwrap().isVisible());
            assert_eq!(
                pane.native.web_view.as_super().frame().size.height,
                f64::from(pane.web_view.height())
            );
            inspector::close(&pane.native.web_view);
            wait(|| unsafe {
                let visible: Bool = msg_send![&*inspector, isVisible];
                !visible.as_bool()
            })
            .await;
        }
        button.emit_clicked();
        wait(|| unsafe {
            let visible: Bool = msg_send![&*inspector, isVisible];
            visible.as_bool()
        })
        .await;
        pane.prepare_for_close();
        wait(|| unsafe {
            let visible: Bool = msg_send![&*inspector, isVisible];
            !visible.as_bool()
        })
        .await;
        window.destroy();
        println!("MACOS_BROWSER_INSPECTOR_OK");
    }
}

async fn wait(mut ready: impl FnMut() -> bool) {
    glib::future_with_timeout(Duration::from_secs(30), async {
        while !ready() {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Inspector window did not reach expected native state");
}

fn button(widget: &gtk::Widget) -> Option<gtk::Button> {
    if widget.widget_name() == "flowmux-web-inspector" {
        return widget.clone().downcast().ok();
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Some(found) = button(&widget) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}
