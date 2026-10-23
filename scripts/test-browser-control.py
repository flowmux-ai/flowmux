#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise pane browser controls through the real CLI in a private GUI process."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Fixture(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"""<!doctype html><title>Browser control fixture</title>
        <button id=target onclick="window.clicks++">Original</button>
        <input id=field aria-label=Field><input type=checkbox id=check>
        <script>window.clicks=0;window.mutations=[];
        new MutationObserver(xs=>mutations.push(...xs)).observe(document.documentElement,
          {subtree:true,childList:true,attributes:true,characterData:true});</script>"""
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


def exercise(cli, url, root):
    workspace = cli("workspace", "new", "--name", "Browser control test", "--root", str(root))
    workspace = workspace["workspace_created"]["id"]
    tree = cli("tree")["tree"]["workspaces"]
    terminal = next(w for w in tree if w["id"] == workspace)["panes"][0]["id"]
    opened = cli("browser", "open", url, pane_context=terminal)
    pane = opened["browser_pane_opened"]["pane"]

    def browser(*args, **kwargs):
        return cli("browser", args[0], pane, *args[1:], **kwargs)

    def evaluate(source):
        return browser("eval", source)["browser_result"]["value"]

    def snapshot():
        return json.loads(browser("snapshot")["browser_result"]["value"])

    def reference(snap, name):
        return next(key for key, meta in snap["refs"].items() if meta["name"] == name)

    assert browser("wait", "--url", url)["browser_bool_result"]["value"]
    assert browser("wait", "--ready-state", "complete")["browser_bool_result"]["value"]
    before = evaluate("document.documentElement.outerHTML")
    first = snapshot()
    original = reference(first, "Original")
    assert evaluate("document.documentElement.outerHTML") == before
    assert evaluate("String(mutations.length)") == "0", "snapshot mutated the DOM"
    browser("click", "@" + original)
    assert evaluate("String(clicks)") == "1"
    browser("fill", reference(first, "Field"), "retained")
    assert browser("value", reference(first, "Field"))["browser_result"]["value"] == "retained"
    assert evaluate("typeof globalThis.__flowmuxRefs") == "undefined"
    evaluate("globalThis.__flowmuxRefs = new Map(); 'page namespace cannot overwrite refs'")

    # Same selector and text, different node: the old observation must not act on it.
    evaluate("document.querySelector('#target').outerHTML = document.querySelector('#target').outerHTML; 'replaced'")
    browser("click", original, error=True)
    assert evaluate("String(clicks)") == "1", "stale ref clicked a replacement"
    second = snapshot()
    browser("click", reference(second, "Original"))
    assert evaluate("String(clicks)") == "2"
    browser("click", original, error=True)
    third = snapshot()
    first_surface = third["page"].get("surface_id")
    browser("click", reference(second, "Original"), error=True)
    browser("click", reference(third, "Original"))
    assert evaluate("String(clicks)") == "3"

    # Another tab's snapshot must never make the previous tab's tokens valid.
    opened = cli("browser", "open", url, pane_context=terminal)
    assert opened["browser_pane_opened"]["pane"] == pane
    assert browser("wait", "--url", url)["browser_bool_result"]["value"]
    assert browser("wait", "--ready-state", "complete")["browser_bool_result"]["value"]
    other = snapshot()
    browser("click", reference(third, "Original"), error=True)
    assert evaluate("String(clicks)") == "0", "old tab ref reached new tab"
    browser("click", reference(other, "Original"))
    assert evaluate("String(clicks)") == "1"
    print("PASS: stale nodes, snapshots and tabs rejected; live actions and non-mutating snapshots preserved", flush=True)
    assert first_surface, "snapshot must identify its browser tab"
    browser("click", reference(third, "Original"), "--surface", first_surface)
    assert browser("eval", "String(clicks)", "--surface", first_surface)["browser_result"]["value"] == "4"
    assert evaluate("String(clicks)") == "1", "pinned action changed the active tab"
    browser("fill", reference(third, "Field"), "pinned", "--surface", first_surface)
    assert browser("value", reference(third, "Field"), "--surface", first_surface)["browser_result"]["value"] == "pinned"
    assert evaluate("document.querySelector('#field').value") == ""
    cli("browser", "url", terminal, "--surface", first_surface, error=True)
    cli("close-tab", first_surface, "--pane", pane)
    browser("type", "must not reach other tab", "--surface", first_surface, error=True)
    assert evaluate("document.querySelector('#field').value") == ""
    print("PASS: explicit browser tab targeting survives active-tab changes and rejects closed or foreign tabs", flush=True)

    button = reference(other, "Original")
    field = reference(other, "Field")
    for style in ["display:none", "visibility:hidden", "opacity:0", "pointer-events:none"]:
        evaluate(f"document.querySelector('#target').style.cssText={json.dumps(style)}; 'hidden'")
        browser("click", button, error=True)
        assert evaluate("String(clicks)") == "1", "inaccessible button was clicked"
    evaluate("document.querySelector('#target').style.cssText=''; 'visible'")
    evaluate("document.body.insertAdjacentHTML('beforeend', '<div id=cover style=\"position:fixed;inset:0;z-index:99\"></div>'); 'covered'")
    browser("click", button, error=True)
    assert evaluate("String(clicks)") == "1"
    evaluate("document.querySelector('#cover').remove(); 'uncovered'")
    evaluate("document.querySelector('#target').disabled=true; 'disabled'")
    browser("click", button, error=True)
    evaluate("window.hovers=0; document.querySelector('#target').onmouseover=()=>hovers++; 'listen'")
    browser("hover", button)
    assert evaluate("String(hovers)") == "1", "disabled controls still support hover"
    evaluate("document.querySelector('#target').disabled=false; 'enabled'")
    for attribute in ["aria-disabled", "inert"]:
        evaluate(f"document.body.setAttribute('{attribute}', 'true'); 'blocked'")
        browser("click", button, error=True)
        evaluate(f"document.body.removeAttribute('{attribute}'); 'unblocked'")
    evaluate("document.querySelector('#target').insertAdjacentHTML('beforebegin', '<fieldset id=group disabled></fieldset>'); document.querySelector('#group').append(document.querySelector('#target')); 'disabled fieldset'")
    browser("click", button, error=True)
    evaluate("document.querySelector('#group').replaceWith(document.querySelector('#target')); 'unwrapped'")
    browser("click", button)
    assert evaluate("String(clicks)") == "2"
    for attribute in ["disabled", "readonly"]:
        evaluate(f"document.querySelector('#field').setAttribute('{attribute}', ''); 'locked'")
        browser("fill", field, "must not be written", error=True)
        assert evaluate("document.querySelector('#field').value") == ""
        evaluate(f"document.querySelector('#field').removeAttribute('{attribute}'); 'unlocked'")
    browser("fill", button, "must not change button", error=True)
    evaluate("document.querySelector('#target').onfocus=()=>document.querySelector('#target').blur(); 'refuse focus'")
    browser("focus", button, error=True)
    evaluate("document.querySelector('#target').onfocus=null; 'allow focus'")
    browser("focus", field)
    assert evaluate("document.activeElement.id") == "field"
    browser("fill", field, "editable")
    assert evaluate("document.querySelector('#field').value") == "editable"
    browser("select", button, "invalid", error=True)
    evaluate("document.body.insertAdjacentHTML('beforeend', '<select id=choice aria-label=Choice><option>one</option><option disabled>blocked</option><option>two</option></select>'); 'select'")
    choice = reference(snapshot(), "Choice")
    browser("select", choice, "blocked", error=True)
    assert evaluate("document.querySelector('#choice').value") == "one"
    browser("select", choice, "two")
    assert evaluate("document.querySelector('#choice').value") == "two"
    evaluate("document.querySelector('#choice').onchange=e=>e.target.value='one'; 'reject change'")
    browser("select", choice, "two", error=True)
    print("PASS: hidden, covered, disabled and readonly targets reject actions without effects", flush=True)

    result = browser("eval-async", "return await new Promise(resolve => setTimeout(() => resolve('finished'), 50))", "--surface", other["page"]["surface_id"])
    assert result["browser_result"]["value"] == "finished"
    rejected = browser("eval-async", "return Promise.reject(new Error('expected rejection'))", error=True)
    assert "expected rejection" in rejected["error"]["message"], rejected
    start = time.monotonic()
    expired = browser("eval-async", "return new Promise(resolve => setTimeout(() => { window.asyncLate = true; resolve('late'); }, 1000))", "--timeout-ms", "100", error=True)
    assert "timed out" in expired["error"]["message"], expired
    assert .08 <= time.monotonic() - start < 5, "async execution did not honor its timeout"
    assert evaluate("'still responsive'") == "still responsive"
    assert browser("wait", "--js", "window.asyncLate === true")["browser_bool_result"]["value"]
    print("PASS: async results settle, rejection is reported and timeout keeps the browser responsive", flush=True)


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/flowmux").resolve()
    cli_binary = binary.with_name("flowmuxctl")
    with tempfile.TemporaryDirectory(prefix="fm-bc-", dir="/tmp") as directory:
        root = Path(directory)
        env = {k: v for k, v in os.environ.items() if not k.startswith("FLOWMUX_")}
        for key, child in [("FLOWMUX_RUNTIME_DIR", "run"), ("XDG_RUNTIME_DIR", "run"),
                           ("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                           ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache")]:
            (root / child).mkdir(mode=0o700, exist_ok=True)
            env[key] = str(root / child)
        env["SHELL"] = "/bin/sh"
        env["GSK_RENDERER"] = "cairo"
        env["FLOWMUX_BUNDLED_CLI_PATH"] = str(cli_binary)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        with (root / "gui.log").open("w+") as log:
            process = subprocess.Popen([str(binary)], cwd=root, env=env, stdout=log, stderr=log)
            socket_path = root / "run" / f"flowmux-{process.pid}.sock"

            def cli(*args, error=False, pane_context=None):
                result = subprocess.run([str(cli_binary), "--socket", str(socket_path), "--json", *args],
                                        env={**env, **({"FLOWMUX_PANE_ID": pane_context} if pane_context else {})},
                                        capture_output=True, text=True, timeout=30)
                assert bool(result.returncode) == error, (args, result.stdout, result.stderr)
                response = json.loads(result.stdout)
                if error:
                    assert "error" in response, response
                return response

            try:
                deadline = time.monotonic() + 30
                while not socket_path.exists():
                    assert process.poll() is None, "GUI exited during startup"
                    assert time.monotonic() < deadline, "GUI startup timed out"
                    time.sleep(.05)
                assert cli("ping") == "pong"
                exercise(cli, f"http://127.0.0.1:{server.server_port}/", root)
                assert process.poll() is None, "test GUI exited unexpectedly"
            finally:
                # Only the process created above is owned by this test.
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                    raise AssertionError("test GUI did not stop")
                finally:
                    server.shutdown()
                    server.server_close()
                    thread.join()
                    if sys.exc_info()[0] is not None:
                        log.seek(0)
                        print(log.read())
            assert process.returncode in (0, -signal.SIGTERM), process.returncode
    print("BROWSER_CONTROL_OK", flush=True)


if __name__ == "__main__":
    main()
