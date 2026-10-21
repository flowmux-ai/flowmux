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
    fg: RGBA,
    link: RGBA,
}

impl Theme {
    pub(super) fn load() -> Self {
        use flowmux_config::theme::{self, DEFAULT_BG, DEFAULT_FG};
        let cfg = theme::resolve(&flowmux_config::options::load(), theme::load());
        let color = |value: Option<&str>, fallback| {
            value
                .and_then(|value| RGBA::parse(value).ok())
                .unwrap_or_else(|| RGBA::parse(fallback).unwrap())
        };
        Self {
            bg: color(cfg.background.as_deref(), DEFAULT_BG),
            fg: color(cfg.foreground.as_deref(), DEFAULT_FG),
            link: color(cfg.palette[4].as_deref(), "#81a2be"),
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
            web.setUnderPageBackgroundColor(Some(&color));
        }
    }

    pub(super) fn css(&self) -> String {
        let scheme = if self.is_dark() { "dark" } else { "light" };
        let error = if self.is_dark() { "#ff8f87" } else { "#b42318" };
        format!(
            r#":root {{
  --bg: {}; --fg: {}; --link: {};
  --muted: color-mix(in srgb, var(--fg) 70%, var(--bg));
  --border: color-mix(in srgb, var(--fg) 25%, var(--bg));
  --code-bg: color-mix(in srgb, var(--fg) 8%, var(--bg));
}}
html, body {{ background: var(--bg); color: var(--fg); color-scheme: {scheme}; }}
.mermaid-error {{ color: {error}; }}"#,
            self.bg, self.fg, self.link
        )
    }

    pub(super) fn document(&self, html: String) -> String {
        let style = format!("<style id=\"flowmux-theme\">{}</style>", self.css());
        if html.contains("</head>") {
            html.replacen("</head>", &(style + "</head>"), 1)
        } else {
            html + &style
        }
    }
}
