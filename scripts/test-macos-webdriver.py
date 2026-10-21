#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run native macOS WebDriver against an isolated process, never an existing app."""
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location("webkit_smoke", Path(__file__).with_name("test-webkit-webdriver.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class Fixture(smoke.Fixture):
    def do_GET(self):
        if self.path == "/broken":
            self.connection.sendall(b"invalid HTTP response\r\n\r\n")
            self.close_connection = True
            return
        if self.path == "/slow":
            time.sleep(.5)
        try:
            super().do_GET()
        except (BrokenPipeError, ConnectionResetError):
            pass  # A timed-out navigation deliberately closes its request.


def expect_error(driver, method, path, body, expected):
    try:
        driver.call(method, path, body)
    except RuntimeError as error:
        assert f'"error":"{expected}"' in str(error), str(error)
    else:
        raise AssertionError(f"{path}: expected {expected}")


def boundaries(driver, url):
    driver.start()
    driver.call("POST", "/url", {"url": url})
    assert driver.call("GET", "/title") == "Flowmux WebDriver test"
    assert "<form" in driver.call("GET", "/source")
    element = driver.element("#input")
    ref = {smoke.ELEMENT: element}
    assert driver.call("POST", "/execute/sync", {
        "script": "return [arguments[0], {again: arguments[0]}]", "args": [ref]
    }) == [ref, {"again": ref}]
    assert driver.evaluate("return Promise.resolve(42)") == 42
    driver.call("POST", "/timeouts", {"script": 50, "implicit": 0})
    expect_error(driver, "POST", "/execute/sync", {"script": "return new Promise(()=>{})", "args": []}, "script timeout")
    driver.call("POST", "/timeouts", {"script": 10000})
    expect_error(driver, "POST", "/element", {"using": "css selector", "value": "["}, "invalid selector")
    expect_error(driver, "POST", "/element", {"using": "css selector", "value": ".absent"}, "no such element")
    assert driver.call("POST", "/elements", {"using": "css selector", "value": ".absent"}) == []
    driver.evaluate("document.querySelector('#input').remove()")
    expect_error(driver, "GET", f"/element/{element}/text", None, "stale element reference")
    driver.call("POST", "/url", {"url": url + "seed"})
    expect_error(driver, "GET", f"/element/{element}/text", None, "stale element reference")
    cookies = driver.call("GET", "/cookie")
    assert any(c["name"] == "flowmux_test" and c["value"] == "private" for c in cookies), cookies
    assert driver.call("GET", "/cookie/flowmux_test")["value"] == "private"
    driver.call("POST", "/back", {})
    assert driver.call("GET", "/url") == url
    driver.call("POST", "/forward", {})
    assert driver.call("GET", "/url") == url + "seed"
    driver.call("POST", "/refresh", {})
    field = driver.element("#input")
    assert driver.call("GET", f"/element/{field}/name") == "input"
    assert driver.call("GET", f"/element/{field}/enabled") is True
    assert driver.call("GET", f"/element/{field}/displayed") is True
    assert driver.call("GET", f"/element/{field}/attribute/id") == "input"
    assert driver.call("GET", f"/element/{field}/property/value") == ""
    assert driver.call("GET", f"/element/{field}/rect")["width"] > 0
    assert driver.call("POST", "/element", {"using": "xpath", "value": "//input"}) == {smoke.ELEMENT: field}
    driver.click("#input")
    driver.call("POST", "/actions", {"actions": [{"type": "key", "id": "keyboard", "actions": [{"type": "keyDown", "value": "\ue008"}]}]})
    driver.call("DELETE", "/actions")
    driver.key("x")
    assert driver.evaluate("return document.querySelector('#input').value") == "x"
    driver.call("POST", f"/element/{field}/clear", {})
    driver.call("POST", f"/element/{field}/value", {"text": "\ue008a\ue000b"})
    shifted = driver.evaluate("return document.querySelector('#input').value")
    assert shifted == "Ab", shifted
    driver.call("POST", f"/element/{field}/value", {"text": "123456789012345678901234567890"})
    driver.call("POST", f"/element/{field}/value", {"text": "TAIL"})
    assert driver.evaluate("return document.querySelector('#input').value") == "Ab123456789012345678901234567890TAIL"
    driver.call("POST", f"/element/{field}/clear", {})
    driver.call("POST", f"/element/{driver.element('#input')}/value", {"text": "한글 café"})
    driver.call("POST", f"/element/{driver.element('#input')}/clear", {})
    assert driver.evaluate("return document.querySelector('#input').value") == ""
    driver.evaluate("document.querySelector('#input').disabled=true")
    expect_error(driver, "POST", f"/element/{driver.element('#input')}/value", {"text": "bad"}, "element not interactable")
    driver.evaluate("document.querySelector('#input').style.display='none'")
    expect_error(driver, "POST", f"/element/{driver.element('#input')}/click", {}, "element not interactable")
    expect_error(driver, "POST", "/window", {"handle": "missing"}, "no such window")
    expect_error(driver, "POST", "/url", {"url": "file:///etc/passwd"}, "invalid argument")
    expect_error(driver, "POST", "/actions", {"actions": [{"type": "wheel", "id": "wheel", "actions": []}]}, "unsupported operation")
    assert driver.call("DELETE", "/window") == []
    driver.session = None
    driver.start()
    driver.stop()
    for headers in [{"Host": "example.com"}, {"Origin": "http://example.com"}]:
        try:
            urlopen(Request(driver.url + "/status", headers=headers), timeout=5)
        except HTTPError as error:
            assert error.code in (400, 403, 500), error.code
        else:
            raise AssertionError(f"Unsafe headers accepted: {headers}")
    try:
        driver.request("GET", "/session/missing/url")
    except RuntimeError as error:
        assert 'invalid session id' in str(error), str(error)
    else:
        raise AssertionError("Unknown session accepted")
    try:
        driver.request("POST", "/session", {"capabilities": {"alwaysMatch": {"browserName": "Chrome"}}})
    except RuntimeError as error:
        assert 'session not created' in str(error), str(error)
    else:
        raise AssertionError("Incompatible browser accepted")
    session = driver.request("POST", "/session", {"capabilities": {"alwaysMatch": {
        "browserName": "Flowmux", "pageLoadStrategy": "eager", "se:remoteUrl": driver.url
    }}})
    assert session["capabilities"]["pageLoadStrategy"] == "eager"
    driver.session = "/session/" + session["sessionId"]
    driver.call("POST", "/url", {"url": url})
    driver.evaluate("document.body.insertAdjacentHTML('beforeend', '<a id=next href=/linked>Next</a>')")
    driver.click("#next")
    assert driver.call("GET", "/url") == url + "linked"
    driver.call("POST", "/timeouts", {"pageLoad": 10000})
    expect_error(driver, "POST", "/url", {"url": url + "broken"}, "unknown error")
    driver.call("POST", "/timeouts", {"pageLoad": 50})
    expect_error(driver, "POST", "/url", {"url": url + "slow"}, "timeout")
    driver.call("POST", "/timeouts", {"pageLoad": 10000})
    driver.call("POST", "/url", {"url": url})
    driver.stop()
    print("PASS: native clear, navigation, cookies, references, timeouts and protocol boundaries", flush=True)


def main():
    if sys.platform != "darwin":
        raise SystemExit("This test requires macOS")
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/flowmux").resolve())
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix="fm-wd-", dir="/tmp") as directory:
        env = {k: v for k, v in os.environ.items() if not k.startswith("FLOWMUX_")}
        env["FLOWMUX_WEBDRIVER_SERVER"] = f"127.0.0.1:{port}"
        for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "FLOWMUX_RUNTIME_DIR"]:
            env[key] = directory + "/" + key
        server = smoke.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        with open(directory + "/server.log", "w+") as log:
            process = subprocess.Popen([binary], env=env, stdout=log, stderr=log)
            driver = smoke.Driver(f"http://127.0.0.1:{port}")
            try:
                deadline = time.monotonic() + 15
                while True:
                    assert process.poll() is None, "Driver exited during startup"
                    if "Flowmux WebDriver listening on " not in Path(log.name).read_text():
                        assert time.monotonic() < deadline, "Driver startup timed out"
                        time.sleep(.05)
                        continue
                    try:
                        assert driver.request("GET", "/status")["ready"]
                        break
                    except OSError:
                        assert time.monotonic() < deadline, "Driver startup timed out"
                        time.sleep(.05)
                fixture = f"http://127.0.0.1:{server.server_port}/"
                smoke.exercise(driver, fixture, None)
                driver.stop()
                boundaries(driver, fixture)
            finally:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                    raise AssertionError("Driver did not stop")
                server.shutdown()
                server.server_close()
                thread.join()
                log.seek(0)
                print(log.read())
            assert process.returncode == 0, process.returncode
    print("MACOS_WEBDRIVER_OK", flush=True)


if __name__ == "__main__":
    main()
