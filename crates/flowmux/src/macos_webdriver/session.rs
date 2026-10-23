// SPDX-License-Identifier: GPL-3.0-or-later
use super::{
    capabilities::Browser,
    native::{self, Browsers, Window},
};
use objc2_app_kit::NSEventModifierFlags as Flags;
use objc2_foundation::{NSPoint, NSString, NSURLRequest, NSURL};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    rc::Rc,
    time::{Duration, Instant},
};
use webdriver::{
    actions::{ActionsType, GeneralAction, KeyAction, KeyActionItem},
    capabilities::CapabilitiesMatching,
    command::*,
    common::WebElement,
    error::{ErrorStatus, WebDriverError, WebDriverResult},
    response::*,
};

type Result<T> = WebDriverResult<T>;
fn error(status: ErrorStatus, message: impl Into<String>) -> WebDriverError {
    WebDriverError::new(status, message.into())
}
fn unsupported() -> WebDriverError {
    error(
        ErrorStatus::UnsupportedOperation,
        "This command is not supported by Flowmux macOS WebDriver",
    )
}
fn value(value: impl Into<Value>) -> WebDriverResponse {
    WebDriverResponse::Generic(ValueResponse(value.into()))
}
fn native_error(message: String) -> WebDriverError {
    error(ErrorStatus::UnknownError, message)
}

#[derive(Default)]
pub(super) struct Driver {
    session: Option<Session>,
}
struct Session {
    browsers: Browsers,
    current: String,
    implicit: u64,
    page_load: u64,
    script: u64,
    strategy: String,
    keys: BTreeSet<String>,
}
impl Driver {
    pub fn handle(
        &mut self,
        command: WebDriverCommand<VoidWebDriverExtensionCommand>,
    ) -> Result<WebDriverResponse> {
        use WebDriverCommand::*;
        match command {
            Status => {
                return Ok(value(
                    json!({"ready": self.session.is_none(), "message": "Flowmux macOS WebDriver"}),
                ))
            }
            DeleteSession => {
                self.session = None;
                return Ok(WebDriverResponse::DeleteSession);
            }
            NewSession(parameters) => {
                let caps = parameters.match_browser(&mut Browser)?.ok_or_else(|| {
                    error(
                        ErrorStatus::SessionNotCreated,
                        "Capabilities do not match Flowmux macOS",
                    )
                })?;
                let strategy = caps
                    .get("pageLoadStrategy")
                    .and_then(Value::as_str)
                    .unwrap_or("normal")
                    .to_string();
                // shortcut: native prompt handling is not exposed; reject requested policies until it is.
                if caps.contains_key("unhandledPromptBehavior") {
                    return Err(unsupported());
                }
                let browsers = Browsers::new();
                let current = browsers.open();
                let mut session = Session {
                    browsers,
                    current,
                    implicit: 0,
                    page_load: 300_000,
                    script: 30_000,
                    strategy,
                    keys: BTreeSet::new(),
                };
                if let Some(timeouts) = caps.get("timeouts") {
                    session.timeouts(serde_json::from_value(timeouts.clone())?)?;
                }
                let response = json!({"browserName": "Flowmux", "browserVersion": env!("CARGO_PKG_VERSION"),
                    "platformName": "mac", "acceptInsecureCerts": false, "setWindowRect": false,
                    "pageLoadStrategy": session.strategy, "timeouts": session.timeout_value(),
                    "flowmux:automation": "native-wkwebview"});
                self.session = Some(session);
                return Ok(WebDriverResponse::NewSession(NewSessionResponse::new(
                    uuid::Uuid::new_v4().to_string(),
                    response,
                )));
            }
            _ => {}
        }
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| error(ErrorStatus::InvalidSessionId, "No active session"))?;
        session.handle(command)
    }
}
impl Session {
    fn window(&self) -> Result<Rc<Window>> {
        self.browsers
            .windows
            .borrow()
            .get(&self.current)
            .filter(|w| w.window.isVisible())
            .cloned()
            .ok_or_else(|| {
                error(
                    ErrorStatus::NoSuchWindow,
                    "Current automation window is closed",
                )
            })
    }
    fn handles(&self) -> Vec<String> {
        self.browsers
            .windows
            .borrow_mut()
            .retain(|_, w| w.window.isVisible());
        self.browsers.windows.borrow().keys().cloned().collect()
    }
    fn timeout_value(&self) -> Value {
        json!({"implicit": self.implicit, "pageLoad": self.page_load, "script": self.script})
    }
    fn timeouts(&mut self, parameters: TimeoutsParameters) -> Result<()> {
        // Bounded waits also keep shutdown responsive after a disconnected client.
        for timeout in [parameters.implicit, parameters.page_load, parameters.script]
            .into_iter()
            .flatten()
        {
            if timeout.is_none_or(|v| v > 300_000) {
                return Err(error(
                    ErrorStatus::InvalidArgument,
                    "Timeouts must be finite and at most 300000 ms",
                ));
            }
        }
        if let Some(Some(v)) = parameters.implicit {
            self.implicit = v;
        }
        if let Some(Some(v)) = parameters.page_load {
            self.page_load = v;
        }
        if let Some(Some(v)) = parameters.script {
            self.script = v;
        }
        Ok(())
    }
    fn atom(&self, body: &str) -> Result<Value> {
        self.atom_timeout(body, Duration::from_secs(10))
    }
    fn atom_timeout(&self, body: &str, timeout: Duration) -> Result<Value> {
        let window = self.window()?;
        let expression = format!("(() => {{const registryName={};{};try {{return {{value:encode((() => {{{body}}})())}};}} catch(e) {{return {{error:e.error || 'javascript error', message:e.message || String(e)}};}} }})()", json!(window.registry), include_str!("atoms.js"));
        let result = window.evaluate_timeout(&expression, timeout).map_err(|e| {
            error(
                if e.contains("timed out") {
                    ErrorStatus::ScriptTimeout
                } else {
                    ErrorStatus::JavascriptError
                },
                e,
            )
        })?;
        if let Some(status) = result.get("error").and_then(Value::as_str) {
            let status = match status {
                "stale element reference" => ErrorStatus::StaleElementReference,
                "invalid selector" => ErrorStatus::InvalidSelector,
                "element not interactable" => ErrorStatus::ElementNotInteractable,
                "element click intercepted" => ErrorStatus::ElementClickIntercepted,
                _ => ErrorStatus::JavascriptError,
            };
            return Err(error(
                status,
                result["message"].as_str().unwrap_or("Script failed"),
            ));
        }
        Ok(result["value"].clone())
    }
    fn element(&self, element: &WebElement, expression: &str) -> Result<Value> {
        self.atom(&format!(
            "const node=element({});return {expression};",
            json!(element.0)
        ))
    }
    fn find(
        &self,
        root: Option<WebElement>,
        parameters: LocatorParameters,
        many: bool,
    ) -> Result<WebDriverResponse> {
        let root = root
            .map(|r| format!("element({})", json!(r.0)))
            .unwrap_or("document".into());
        let source = format!(
            "return find({root},{},{});",
            json!(parameters.using),
            json!(parameters.value)
        );
        let deadline = Instant::now() + Duration::from_millis(self.implicit);
        loop {
            let nodes = self.atom(&source)?;
            if let Some(node) = nodes.as_array().and_then(|nodes| nodes.first()) {
                return Ok(value(if many { nodes } else { node.clone() }));
            }
            if Instant::now() >= deadline {
                return if many {
                    Ok(value(json!([])))
                } else {
                    Err(error(ErrorStatus::NoSuchElement, "No matching element"))
                };
            }
            native::pump();
        }
    }
    fn click(&self, element: &WebElement) -> Result<()> {
        let point = self.element(element, "center(node)")?;
        self.window()?
            .click(NSPoint::new(
                point["x"].as_f64().ok_or_else(|| {
                    error(ErrorStatus::JavascriptError, "Invalid click coordinates")
                })?,
                point["y"].as_f64().ok_or_else(|| {
                    error(ErrorStatus::JavascriptError, "Invalid click coordinates")
                })?,
            ))
            .map_err(native_error)?;
        self.atom("return null;")?;
        Ok(())
    }
    fn flags(&self) -> Flags {
        let mut flags = Flags::empty();
        for (key, flag) in [
            ("\u{e008}", Flags::Shift),
            ("\u{e009}", Flags::Control),
            ("\u{e00a}", Flags::Option),
            ("\u{e03d}", Flags::Command),
        ] {
            if self.keys.contains(key) {
                flags |= flag;
            }
        }
        flags
    }
    fn key(&mut self, key: &str, down: bool) -> Result<()> {
        let (text, code) = key_code(key)?;
        if down {
            self.keys.insert(key.into());
        } else {
            self.keys.remove(key);
        }
        let window = self.window()?;
        if code == u16::MAX {
            if down {
                window.text(text).map_err(native_error)?;
            }
        } else {
            window
                .key(text, code, down, self.flags())
                .map_err(native_error)?;
        }
        self.atom("return null;")?;
        Ok(())
    }
    fn release(&mut self) -> Result<()> {
        for key in self
            .keys
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            self.key(&key, false)?;
        }
        Ok(())
    }
    fn loaded(&self) -> Result<()> {
        if self.strategy == "none" {
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_millis(self.page_load);
        loop {
            let window = self.window()?;
            let ready = self.atom("return document.readyState;")?;
            if let Some(message) = window.navigation_error.borrow().clone() {
                return Err(native_error(message));
            }
            if (!unsafe { window.web.isLoading() } && ready == "complete")
                || (self.strategy == "eager"
                    && window.committed.get()
                    && (ready == "interactive" || ready == "complete"))
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                unsafe {
                    window.web.stopLoading();
                }
                return Err(error(ErrorStatus::Timeout, "Navigation timed out"));
            }
            native::pump();
        }
    }
    fn handle(
        &mut self,
        command: WebDriverCommand<VoidWebDriverExtensionCommand>,
    ) -> Result<WebDriverResponse> {
        use WebDriverCommand::*;
        match command {
            GetWindowHandles => return Ok(value(json!(self.handles()))),
            GetWindowHandle => {
                self.window()?;
                return Ok(value(self.current.clone()));
            }
            SwitchToWindow(p) => {
                if !self.handles().contains(&p.handle) {
                    return Err(error(
                        ErrorStatus::NoSuchWindow,
                        "Unknown automation window",
                    ));
                }
                self.release().or_else(|e| {
                    if e.error == ErrorStatus::NoSuchWindow {
                        self.keys.clear();
                        Ok(())
                    } else {
                        Err(e)
                    }
                })?;
                self.current = p.handle;
                self.window()?.window.makeKeyAndOrderFront(None);
                return Ok(WebDriverResponse::Void);
            }
            GetTimeouts => return Ok(value(self.timeout_value())),
            SetTimeouts(p) => {
                self.timeouts(p)?;
                return Ok(WebDriverResponse::Void);
            }
            _ => {}
        }
        let window = self.window()?;
        let result = match command {
            NewWindow(_) => {
                let handle = self.browsers.open();
                // WKWebView automation owns native windows; a tab request is a hint.
                return Ok(WebDriverResponse::NewWindow(NewWindowResponse { handle, typ: "window".into() }));
            }
            CloseWindow => {
                self.release()?;
                self.browsers.windows.borrow_mut().remove(&self.current);
                drop(window);
                return Ok(WebDriverResponse::CloseWindow(CloseWindowResponse(self.handles())));
            }
            Get(p) => {
                let parsed = reqwest::Url::parse(&p.url).map_err(|_| error(ErrorStatus::InvalidArgument, "Invalid URL"))?;
                if !matches!(parsed.scheme(), "http" | "https" | "about" | "data") { return Err(error(ErrorStatus::InvalidArgument, "Only http, https, about and data URLs are supported")); }
                let url = NSURL::URLWithString(&NSString::from_str(&p.url)).ok_or_else(|| error(ErrorStatus::InvalidArgument, "Invalid URL"))?;
                window.committed.set(false);
                *window.navigation_error.borrow_mut() = None;
                unsafe { window.web.loadRequest(&NSURLRequest::requestWithURL(&url)); }
                self.loaded()?; Value::Null
            }
            GoBack | GoForward | Refresh => {
                unsafe { match command { GoBack => { window.web.goBack(); }, GoForward => { window.web.goForward(); }, _ => { window.web.reload(); } } }
                self.loaded()?; Value::Null
            }
            GetCurrentUrl => json!(unsafe { window.web.URL() }.and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or("about:blank".into())),
            GetTitle => json!(unsafe { window.web.title() }.map(|s| s.to_string()).unwrap_or_default()),
            GetPageSource => self.atom("return document.documentElement.outerHTML;")?,
            FindElement(p) => return self.find(None, p, false),
            FindElements(p) => return self.find(None, p, true),
            FindElementElement(e, p) => return self.find(Some(e), p, false),
            FindElementElements(e, p) => return self.find(Some(e), p, true),
            GetActiveElement => self.atom("return document.activeElement;")?,
            IsDisplayed(e) => self.element(&e, "shown(node)")?,
            IsEnabled(e) => self.element(&e, "!node.matches(':disabled')")?,
            IsSelected(e) => self.element(&e, "!!(node.selected || node.checked)")?,
            GetElementText(e) => self.element(&e, "node.innerText")?,
            GetElementTagName(e) => self.element(&e, "node.tagName.toLowerCase()")?,
            GetElementAttribute(e, name) => self.element(&e, &format!("node.getAttribute({})", json!(name)))?,
            GetElementProperty(e, name) => self.element(&e, &format!("node[{}]", json!(name)))?,
            GetCSSValue(e, name) => self.element(&e, &format!("getComputedStyle(node).getPropertyValue({})", json!(name)))?,
            GetElementRect(e) => self.element(&e, "(() => {const r=node.getBoundingClientRect();return {x:r.x+scrollX,y:r.y+scrollY,width:r.width,height:r.height}})()")?,
            ElementClick(e) => { self.click(&e)?; self.loaded()?; Value::Null }
            ElementSendKeys(e, p) => {
                if self.element(&e, "node.matches(':disabled') || node.readOnly || node.type === 'file'")? == true { return Err(error(ErrorStatus::ElementNotInteractable, "Element is not editable")); }
                for ch in p.text.chars() { if ch != '\u{e000}' { key_code(&ch.to_string())?; } }
                if self.element(&e, "node !== document.activeElement")? == true {
                    self.click(&e)?;
                    window.end().map_err(native_error)?;
                }
                for ch in p.text.chars() {
                    let key = ch.to_string();
                    if ch == '\u{e000}' { self.release()?; }
                    else { self.key(&key, true)?; if !matches!(ch, '\u{e008}' | '\u{e009}' | '\u{e00a}' | '\u{e03d}') { self.key(&key, false)?; } }
                }
                self.release()?; Value::Null
            }
            ElementClear(e) => {
                if self.element(&e, "node.matches(':disabled') || node.readOnly || node.type === 'file' || !(node.isContentEditable || /^(INPUT|TEXTAREA)$/.test(node.tagName))")? == true { return Err(error(ErrorStatus::InvalidElementState, "Element is not editable")); }
                self.click(&e)?;
                window.clear().map_err(native_error)?;
                self.atom("return null;")?; Value::Null
            }
            ExecuteScript(p) => {
                // Promise completion is polled without blocking the AppKit run loop.
                let token = uuid::Uuid::new_v4().to_string();
                let args = json!(p.args.unwrap_or_default());
                self.atom_timeout(&format!("const pending={{token:{token},done:false}};registry.pending=pending;Promise.resolve((function(){{{script}\n}}).apply(null,decode({args}))).then(v=>{{try{{pending.value=encode(v)}}catch(e){{pending.error=String(e)}}pending.done=true}},e=>{{pending.error=String(e);pending.done=true}});return null;", token=json!(token), script=p.script), Duration::from_millis(self.script))?;
                let deadline = Instant::now() + Duration::from_millis(self.script);
                loop {
                    let result = self.atom("return registry.pending;")?;
                    if result["token"] != token { return Err(error(ErrorStatus::JavascriptError, "Document changed during script")); }
                    if result["done"] == true {
                        if let Some(e) = result["error"].as_str() { return Err(error(ErrorStatus::JavascriptError, e)); }
                        break result["value"].clone();
                    }
                    if Instant::now() >= deadline { return Err(error(ErrorStatus::ScriptTimeout, "Script timed out")); }
                    native::pump();
                }
            }
            PerformActions(p) => {
                // shortcut: one keyboard source; pointer/wheel/multi-source actions fail explicitly.
                if p.actions.len() != 1 { return Err(unsupported()); }
                let sequence = p.actions.into_iter().next().unwrap();
                let ActionsType::Key { actions } = sequence.actions else { return Err(unsupported()); };
                for action in &actions {
                    match action {
                        KeyActionItem::Key(KeyAction::Down(p)) => { key_code(&p.value)?; }
                        KeyActionItem::Key(KeyAction::Up(p)) => { key_code(&p.value)?; }
                        KeyActionItem::General(GeneralAction::Pause(p)) if p.duration.unwrap_or(0) > 300_000 => return Err(error(ErrorStatus::InvalidArgument, "Pause exceeds 300000 ms")),
                        _ => {}
                    }
                }
                for action in actions {
                    match action {
                        KeyActionItem::Key(KeyAction::Down(p)) => self.key(&p.value, true)?,
                        KeyActionItem::Key(KeyAction::Up(p)) => self.key(&p.value, false)?,
                        KeyActionItem::General(GeneralAction::Pause(p)) => {
                            let deadline = Instant::now() + Duration::from_millis(p.duration.unwrap_or(0));
                            while Instant::now() < deadline { native::pump(); }
                        }
                    }
                }
                Value::Null
            }
            ReleaseActions => { self.release()?; Value::Null }
            GetCookies | GetNamedCookie(_) => {
                let location = self.atom("return {host:location.hostname,path:location.pathname,secure:location.protocol==='https:'};")?;
                let host = location["host"].as_str().unwrap_or("");
                let path = location["path"].as_str().unwrap_or("/");
                let mut cookies = Vec::new();
                for cookie in self.browsers.cookies().map_err(native_error)? {
                    let domain = cookie.domain().to_string();
                    let domain = domain.trim_start_matches('.');
                    let cookie_path = cookie.path().to_string();
                    let domain_match = host == domain || (cookie.domain().to_string().starts_with('.') && host.ends_with(&format!(".{domain}")));
                    let path_match = path == cookie_path || path.strip_prefix(&cookie_path).is_some_and(|rest| cookie_path.ends_with('/') || rest.starts_with('/'));
                    if domain_match && path_match && (!cookie.isSecure() || location["secure"] == true) {
                        let mut result = json!({"name":cookie.name().to_string(),"value":cookie.value().to_string(),"domain":cookie.domain().to_string(),"path":cookie_path,"secure":cookie.isSecure(),"httpOnly":cookie.isHTTPOnly()});
                        if let Some(expiry) = cookie.expiresDate() { result["expiry"] = json!(expiry.timeIntervalSince1970() as u64); }
                        cookies.push(result);
                    }
                }
                if let GetNamedCookie(name) = command { cookies.into_iter().find(|c| c["name"] == name).ok_or_else(|| error(ErrorStatus::NoSuchCookie, "Cookie not found"))? } else { json!(cookies) }
            }
            TakeScreenshot => json!(window.screenshot().map_err(native_error)?),
            _ => return Err(unsupported()),
        };
        Ok(value(result))
    }
}

fn key_code(key: &str) -> Result<(&str, u16)> {
    Ok(match key {
        "\u{e003}" => ("\u{7f}", 51),
        "\u{e004}" => ("\t", 48),
        "\u{e006}" | "\u{e007}" | "\n" | "\r" => ("\r", 36),
        "\u{e008}" => ("", 56),
        "\u{e009}" => ("", 59),
        "\u{e00a}" => ("", 58),
        "\u{e03d}" => ("", 55),
        "\u{e00c}" => ("\u{1b}", 53),
        "\u{e00d}" => (" ", 49),
        "\u{e012}" => ("\u{f702}", 123),
        "\u{e013}" => ("\u{f700}", 126),
        "\u{e014}" => ("\u{f703}", 124),
        "\u{e015}" => ("\u{f701}", 125),
        "\u{e017}" => ("\u{f728}", 117),
        "\u{e010}" => ("\u{f72b}", 119),
        "\u{e011}" => ("\u{f729}", 115),
        "\u{e00e}" => ("\u{f72c}", 116),
        "\u{e00f}" => ("\u{f72d}", 121),
        _ if key.chars().any(|c| ('\u{e000}'..='\u{f8ff}').contains(&c)) => {
            return Err(unsupported())
        }
        _ if key.is_ascii() && key.len() == 1 => {
            crate::macos_input::physical_key(key).ok_or_else(unsupported)?
        }
        _ => (key, u16::MAX),
    })
}
