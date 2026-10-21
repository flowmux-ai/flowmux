// SPDX-License-Identifier: GPL-3.0-or-later
use serde_json::{Map, Value};
use webdriver::capabilities::{BrowserCapabilities, Capabilities};
use webdriver::error::{ErrorStatus, WebDriverError, WebDriverResult};

pub(super) struct Browser;
macro_rules! unsupported {
    ($($name:ident),*) => { $(fn $name(&mut self, _: &Capabilities) -> WebDriverResult<bool> { Ok(false) })* };
}
impl BrowserCapabilities for Browser {
    fn init(&mut self, _: &Capabilities) {}
    fn browser_name(&mut self, _: &Capabilities) -> WebDriverResult<Option<String>> {
        Ok(Some("Flowmux".into()))
    }
    fn browser_version(&mut self, _: &Capabilities) -> WebDriverResult<Option<String>> {
        Ok(Some(env!("CARGO_PKG_VERSION").into()))
    }
    fn platform_name(&mut self, _: &Capabilities) -> WebDriverResult<Option<String>> {
        Ok(Some("mac".into()))
    }
    fn compare_browser_version(&mut self, actual: &str, wanted: &str) -> WebDriverResult<bool> {
        Ok(actual == wanted)
    }
    unsupported!(
        accept_insecure_certs,
        set_window_rect,
        strict_file_interactability,
        web_socket_url,
        webauthn_virtual_authenticators,
        webauthn_extension_uvm,
        webauthn_extension_prf,
        webauthn_extension_large_blob,
        webauthn_extension_cred_blob
    );
    fn accept_proxy(
        &mut self,
        proxy: &Map<String, Value>,
        _: &Capabilities,
    ) -> WebDriverResult<bool> {
        Ok(proxy.is_empty())
    }
    fn validate_custom(&mut self, name: &str, value: &Value) -> WebDriverResult<()> {
        if name == "se:remoteUrl" && !value.is_string() {
            return Err(WebDriverError::new(
                ErrorStatus::InvalidArgument,
                "se:remoteUrl must be a string",
            ));
        }
        Ok(())
    }
    fn accept_custom(&mut self, name: &str, _: &Value, _: &Capabilities) -> WebDriverResult<bool> {
        // Selenium supplies its connection URL as metadata; it never changes our listener.
        Ok(name == "se:remoteUrl")
    }
}
