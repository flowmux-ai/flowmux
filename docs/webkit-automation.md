<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# WebKitWebDriver automation (Linux)

Flowmux can expose **new, ephemeral WebKit browser windows** to a standard
WebDriver client such as Selenium. WebKit performs mouse and keyboard input
inside its engine, including native editing, Tab focus and Enter submission.
This uses the same WebKitGTK renderer as Flowmux's normal browser.

This is an opt-in, Linux-only connection. It does not attach to existing pane
tabs or change the JavaScript-based `flowmux browser click/type/press` commands.
WebDriver owns separate windows labelled **Flowmux · WebDriver**; they are not
CLI pane targets and are not restored with workspaces. Both `window` and `tab`
requests create separate windows. Popups remain in the same automation session.
Ending the session closes its windows and discards its cookies/site data.
Normal browser profiles and terminal sessions remain separate.

## Runtime requirements

- WebKitGTK **6.0 API, version 2.46+**, built with WebDriver mouse and keyboard
  interactions enabled (normally `-DENABLE_WEBDRIVER=ON`).
- A compatible `WebKitWebDriver` executable with `--target` support.
- A running graphical session, or Xvfb for isolated tests.

**The installed library's build options matter.** Ubuntu 24.04's WebKitGTK
2.52.6 GTK4 package disables WebDriver. A session can connect and navigate yet
return `unsupported operation` for click/keyboard actions. Installing the driver
executable alone does not enable those features in the library. Use a compatible
runtime built with them enabled; GNOME Platform 49 with WebKitGTK/WebKitWebDriver
2.54.1 was used for the live check.
Flowmux does not silently fall back to JavaScript when a WebDriver action fails.

For isolated Flatpak verification, use an installed test application referencing
that runtime, with private state and a private D-Bus session. Running the runtime
itself as an application lacks the `Application` metadata needed by the Flatpak
spawn portal. Check that `flatpak-spawn --sandbox true` succeeds in the test
application, and reject runs logging `Sandboxed processes will be spawned
without a sandbox`. The portal needs access to the same Flatpak installation as
the test application; a private D-Bus session can inherit its `XDG_DATA_HOME`.
Keep any additional portal permission confined to the test application.

## Connect

Start a new Flowmux instance with automation explicitly enabled. Do not restart
an active agent window merely to enable automation. Choose two free ports:

```bash
FLOWMUX_WEBKIT_AUTOMATION=1 \
WEBKIT_INSPECTOR_SERVER=127.0.0.1:9223 \
flowmux
```

In another terminal, start the driver:

```bash
WebKitWebDriver --host=127.0.0.1 --port=4444 --target=127.0.0.1:9223
```

The inspector address must be a numeric loopback address with a nonzero port,
even when `FLOWMUX_WEBKIT_AUTOMATION` is unset or `0`: WebKit starts its
inspector independently. A disabled automation flag does not disable inspection.
Both endpoints provide browser control without authentication; keep the driver
on loopback too. One driver session may be active at a time.

With Selenium installed in your Python environment:

```python
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.webkitgtk.options import Options

options = Options()
options.set_capability("browserName", "Flowmux")
driver = webdriver.Remote("http://127.0.0.1:4444", options=options)
try:
    driver.get("https://example.com")
    print(driver.find_element(By.TAG_NAME, "h1").text)
    # On a page containing a form:
    # field = driver.find_element(By.CSS_SELECTOR, "input[name=q]")
    # field.click()
    # field.send_keys("검색어", Keys.ENTER)
finally:
    driver.quit()
```

Log in inside the automation session when needed. In the tested WebKitGTK
2.54.1 runtime, WebDriver cookie insertion returned success without making the
cookie available to the page; do not rely on it to import sessions. Cookies set
by websites work and are isolated between sessions. Host Chrome/Firefox cookies
and normal Flowmux profile cookies are not imported.

## Verify native input

Against a **dedicated test instance and driver**, run the stdlib-only smoke test:

```bash
python3 scripts/test-webkit-webdriver.py \
  --driver http://127.0.0.1:4444 --screenshot /tmp/flowmux-webdriver.png
```

The fixture is served on loopback. The test checks trusted mouse/key/input
events, Korean text and deletion, Enter submission, Tab focus, multiple windows,
popups, screenshots, session reconnection and cookie isolation. It closes only
the WebDriver session it creates. This test requires an input-enabled runtime;
the standard Ubuntu CI library cannot provide the native-input checks.

## Compatibility limits

- There is no CDP endpoint. Existing Playwright/Puppeteer CDP attachment code
  cannot connect; automation must use a WebDriver client. Playwright's own
  patched WebKit is a different browser distribution.
- This does not add device emulation, network mocking, or screencasting to
  Flowmux's CLI. Window sizing through WebDriver is not device emulation.
- macOS WKWebView does not expose WebKitGTK's automation-session API.
  SafariDriver controls Safari, not an embedded WKWebView. Native macOS input
  and direct automation of existing pane tabs require separate work.

References: [WebKit automation API](https://webkitgtk.org/reference/webkitgtk/stable/class.AutomationSession.html),
[automation-controlled views](https://webkitgtk.org/reference/webkitgtk/stable/property.WebView.is-controlled-by-automation.html),
[Playwright WebKit](https://playwright.dev/docs/browsers#webkit).
