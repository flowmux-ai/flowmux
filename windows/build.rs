// SPDX-License-Identifier: GPL-3.0-or-later
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Native edit cues require common controls v6. Embed only in the GUI.
        println!("cargo:rustc-link-arg-bin=flowmux=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bin=flowmux=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");
    }
}
