#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise a running, opt-in Flowmux through WebKitWebDriver (stdlib only)."""

import argparse
import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import threading
import time
from urllib.error import HTTPError
from urllib.parse import urlparse
from urllib.request import Request, urlopen


HTML = b"""<!doctype html><meta charset=utf-8><title>Flowmux WebDriver test</title>
<form id=form><input id=input><button id=submit>Submit</button></form>
<button id=popup onclick="window.open('/popup')">Open window</button>
<script>
window.events = []; window.submissions = 0;
for (const name of ['pointerdown', 'mousedown', 'click', 'keydown', 'input'])
  document.addEventListener(name, e => events.push([e.type, e.isTrusted]));
document.querySelector('form').onsubmit = e => { e.preventDefault(); submissions++; };
</script>"""
ELEMENT = "element-6066-11e4-a52e-4f735466cecf"


class Fixture(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        if self.path == "/seed":
            self.send_header("Set-Cookie", "flowmux_test=private; Path=/; SameSite=Lax")
        self.end_headers()
        self.wfile.write(HTML)

    def log_message(self, *_args):
        pass


class Driver:
    def __init__(self, url):
        self.url = url.rstrip("/")
        self.session = None

    def request(self, method, path, body=None):
        request = Request(
            self.url + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
            method=method,
        )
        try:
            with urlopen(request, timeout=30) as response:
                return json.load(response)["value"]
        except HTTPError as error:
            detail = error.read().decode()
            if '"unsupported operation"' in detail:
                raise RuntimeError(
                    "WebKitGTK lacks this WebDriver operation. For click/key failures, "
                    "check that its GTK4 build enables ENABLE_WEBDRIVER and input interactions. "
                    + detail
                ) from error
            raise RuntimeError(f"{method} {path}: {detail}") from error

    def start(self):
        value = self.request("POST", "/session", {
            "capabilities": {"alwaysMatch": {"browserName": "Flowmux"}}
        })
        self.session = "/session/" + value["sessionId"]
        assert value["capabilities"]["browserName"] == "Flowmux"
        self.call("POST", "/timeouts", {"pageLoad": 15000, "script": 10000, "implicit": 5000})

    def call(self, method, path, body=None):
        assert self.session
        return self.request(method, self.session + path, body)

    def stop(self):
        if self.session:
            self.call("DELETE", "")
            self.session = None

    def evaluate(self, script):
        return self.call("POST", "/execute/sync", {"script": script, "args": []})

    def element(self, selector):
        return self.call("POST", "/element", {"using": "css selector", "value": selector})[ELEMENT]

    def click(self, selector):
        self.call("POST", f"/element/{self.element(selector)}/click", {})

    def key(self, key):
        self.call("POST", "/actions", {"actions": [{
            "type": "key", "id": "keyboard", "actions": [
                {"type": "keyDown", "value": key}, {"type": "keyUp", "value": key}
            ]
        }]})


def exercise(driver, fixture_url, screenshot):
    driver.start()
    driver.call("POST", "/url", {"url": fixture_url})
    first = driver.call("GET", "/window")
    assert driver.call("GET", "/window/handles") == [first]
    driver.click("#input")
    driver.call("POST", f"/element/{driver.element('#input')}/value", {"text": "한글 café"})
    driver.key("\ue003")  # Backspace: engine editing, not a JS value assignment.
    assert driver.evaluate("return document.querySelector('#input').value") == "한글 caf"
    driver.key("\ue007")  # Enter must perform the browser's default form submission.
    assert driver.evaluate("return submissions") == 1
    driver.key("\ue004")  # Tab must move native focus.
    assert driver.evaluate("return document.activeElement.id") == "submit"
    events = driver.evaluate("return events")
    for expected in ["pointerdown", "mousedown", "click", "keydown", "input"]:
        assert [expected, True] in events, (expected, events)
    assert all(trusted for _, trusted in events), events
    print("PASS: trusted pointer/key/input events, Unicode editing, Enter, Tab", flush=True)

    driver.call("POST", "/url", {"url": fixture_url + "seed"})
    assert driver.evaluate("return document.cookie") == "flowmux_test=private"
    driver.evaluate("localStorage.setItem('flowmux_test', 'private')")
    second = driver.call("POST", "/window/new", {"type": "tab"})["handle"]
    driver.call("POST", "/window", {"handle": second})
    driver.call("POST", "/url", {"url": fixture_url})
    driver.click("#input")
    driver.key("x")
    assert driver.evaluate("return document.querySelector('#input').value") == "x"
    driver.call("POST", "/window", {"handle": first})
    assert driver.evaluate("return document.querySelector('#input').value") == ""
    driver.click("#popup")
    handles = driver.call("GET", "/window/handles")
    assert len(handles) == 3, handles
    popup = (set(handles) - {first, second}).pop()
    driver.call("POST", "/window", {"handle": popup})
    deadline = time.monotonic() + 10
    while driver.call("GET", "/url") != fixture_url + "popup":
        assert time.monotonic() < deadline, "Popup navigation timed out"
        time.sleep(0.05)
    driver.element("#input")  # window.open navigation completes after the handle appears.
    cookies = driver.evaluate("return document.cookie")
    assert cookies == "flowmux_test=private", cookies
    assert len(driver.call("DELETE", "/window")) == 2
    driver.call("POST", "/window", {"handle": second})
    assert len(driver.call("DELETE", "/window")) == 1
    driver.call("POST", "/window", {"handle": first})
    png = base64.b64decode(driver.call("GET", "/screenshot"))
    assert png.startswith(b"\x89PNG\r\n\x1a\n")
    if screenshot:
        Path(screenshot).write_bytes(png)
    print("PASS: context targeting, popup, close and screenshot", flush=True)
    driver.stop()

    driver.start()
    driver.call("POST", "/url", {"url": fixture_url})
    assert driver.call("GET", "/cookie") == [], "Cookies leaked across sessions"
    assert driver.evaluate("return localStorage.getItem('flowmux_test')") is None
    assert len(driver.call("GET", "/window/handles")) == 1
    driver.click("#popup")
    assert len(driver.call("GET", "/window/handles")) == 2
    driver.stop()  # Quit must also dispose a popup that the client never closed.
    driver.start()
    assert len(driver.call("GET", "/window/handles")) == 1
    print("PASS: session reconnect, cookie/storage isolation and popup disposal", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--driver", required=True, help="Dedicated local WebKitWebDriver URL")
    parser.add_argument("--screenshot", help="Optional PNG output path")
    args = parser.parse_args()
    endpoint = urlparse(args.driver)
    if endpoint.scheme != "http" or endpoint.hostname not in {"127.0.0.1", "::1", "localhost"}:
        parser.error("use a dedicated loopback WebKitWebDriver endpoint")
    server = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    driver = Driver(args.driver)
    try:
        exercise(driver, f"http://127.0.0.1:{server.server_port}/", args.screenshot)
    finally:
        driver.stop()
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == "__main__":
    main()
