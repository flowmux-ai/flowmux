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
        window.present();
        wait(|| pane.native.web_view.as_super().window().is_some()).await;
        assert!(
            unsafe { pane.native.web_view.isInspectable() },
            "browser must be inspectable in release builds"
        );
        let button = button(pane.root.upcast_ref()).expect("Inspector toolbar button");
        let inspector: Retained<AnyObject> =
            unsafe { msg_send![&*pane.native.web_view, _inspector] };
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
