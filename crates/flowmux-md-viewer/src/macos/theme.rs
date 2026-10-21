// SPDX-License-Identifier: GPL-3.0-or-later
use gtk::gdk::RGBA;
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSColor, NSWindow,
};
use objc2_web_kit::WKWebView;

#[derive(PartialEq)]
pub(super) struct Theme {
    bg: RGBA,
}

impl Theme {
    pub(super) fn load() -> Self {
        use flowmux_config::theme::{self, DEFAULT_BG};
        let cfg = theme::resolve(&flowmux_config::options::load(), theme::load());
        Self {
            bg: cfg
                .background
                .as_deref()
                .and_then(|value| RGBA::parse(value).ok())
                .unwrap_or_else(|| RGBA::parse(DEFAULT_BG).unwrap()),
        }
    }

    fn is_dark(&self) -> bool {
        0.2126 * self.bg.red() + 0.7152 * self.bg.green() + 0.0722 * self.bg.blue() < 0.5
    }

    pub(super) fn apply(&self, window: &NSWindow, web: &WKWebView) {
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(
            self.bg.red().into(),
            self.bg.green().into(),
            self.bg.blue().into(),
            1.0,
        );
        window.setBackgroundColor(Some(&color));
        window.setTitlebarAppearsTransparent(true);
        unsafe {
            let name = if self.is_dark() {
                NSAppearanceNameDarkAqua
            } else {
                NSAppearanceNameAqua
            };
            window.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
            // The document stays light independently of the surrounding window theme.
            web.setAppearance(NSAppearance::appearanceNamed(NSAppearanceNameAqua).as_deref());
            web.setUnderPageBackgroundColor(Some(&NSColor::whiteColor()));
        }
    }
}
