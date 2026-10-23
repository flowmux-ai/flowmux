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
        <p><label for=contact>Contact email</label><input id=contact placeholder=Wrong>
        <label>Postal code <input id=postal title=Wrong></label>
        <span id=first hidden>Confirm</span><span id=second hidden> order </span>
        <button id=named aria-labelledby="first second" aria-label=Wrong>Wrong</button>
        <input type=submit value=Send title=Wrong>
        <input type=reset value=Clear title=Wrong>
        <button aria-labelledby=missing aria-label="Fallback name">Wrong</button>
        <button id=self aria-labelledby="self second" aria-label=Self>Wrong</button>
        <button title=Wrong>Visible name</button>
        <textarea title="Message title" placeholder=Wrong>existing message</textarea>
        <label for=multi>First label</label><label for=multi>Second label</label><input id=multi>
        <input type=password value=secret aria-label=Password></p>
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
    assert evaluate("document.documentElement.outerHTML") == before
    assert evaluate("String(mutations.length)") == "0", "snapshot mutated the DOM"
    names = {meta["name"] for meta in first["refs"].values()}
    assert {"Contact email", "Postal code", "Confirm order", "Send", "Clear",
            "Fallback name", "Self order", "Visible name", "Message title",
            "First label Second label", "Password"} <= names, names
    assert "secret" not in first["markdown"]
    assert first["refs"][reference(first, "Clear")]["role"] == "button"
    browser("fill", reference(first, "Contact email"), "agent@example.test")
    assert evaluate("document.querySelector('#contact').value") == "agent@example.test"
    print("PASS: snapshot names follow HTML labels and ARIA reference order", flush=True)
    original = reference(first, "Original")
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
    browser("click", reference(third, "Original"), "--surface", first_surface, error=True)
    assert browser("eval", "String(clicks)", "--surface", first_surface)["browser_result"]["value"] == "3"
    assert evaluate("String(clicks)") == "1", "pinned action changed the active tab"
    browser("fill", reference(third, "Field"), "pinned", "--surface", first_surface)
    assert browser("value", reference(third, "Field"), "--surface", first_surface)["browser_result"]["value"] == "pinned"
    assert evaluate("document.querySelector('#field').value") == ""
    browser("press", "Enter", "--surface", first_surface, error=True)
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
    browser("hover", button)
    assert evaluate("String(document.querySelector('#target').matches(':hover'))") == "true", "disabled controls still support hover"
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
    evaluate("document.querySelector('#target').blur(); document.querySelector('#target').onfocus=()=>document.querySelector('#target').blur(); 'refuse focus'")
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

    current = snapshot()
    field = reference(current, "Field")
    browser("fill", field, "abcDEFghi")
    browser("focus", field)
    evaluate("window.typedEvents=[]; document.querySelector('#field').addEventListener('input', e=>typedEvents.push(e.isTrusted)); document.querySelector('#field').setSelectionRange(3, 6); 'selected'")
    browser("type", "한글😀")
    assert evaluate("document.querySelector('#field').value") == "abc한글😀ghi", "typing did not replace the selection"
    assert evaluate("JSON.stringify(typedEvents)") == "[true]", "typing did not use native editing"
    evaluate("document.querySelector('#field').addEventListener('beforeinput', e=>e.preventDefault(), {once:true}); 'cancel next input'")
    browser("type", "blocked", error=True)
    assert evaluate("document.querySelector('#field').value") == "abc한글😀ghi"
    evaluate("document.querySelector('#field').readOnly=true; 'readonly'")
    browser("type", "blocked", error=True)
    evaluate("document.querySelector('#field').readOnly=false; 'editable'")
    browser("type", "!")
    assert evaluate("document.querySelector('#field').value") == "abc한글😀!ghi", "failed input retained a pending lock"
    evaluate("document.body.insertAdjacentHTML('beforeend', '<div id=editor contenteditable role=textbox aria-label=Editor>abcd</div>'); 'editor'")
    editor = reference(snapshot(), "Editor")
    browser("focus", editor)
    evaluate("const range=document.createRange(); range.setStart(document.querySelector('#editor').firstChild,1); range.setEnd(document.querySelector('#editor').firstChild,3); getSelection().removeAllRanges(); getSelection().addRange(range); 'selected rich text'")
    browser("type", "가😀")
    assert evaluate("document.querySelector('#editor').textContent") == "a가😀d"
    print("PASS: native text insertion replaces selection, emits trusted input and respects cancellation", flush=True)

    evaluate("document.body.insertAdjacentHTML('beforeend', '<form id=keyform><input id=keyfield aria-label=Keyboard><button id=keysubmit>Submit</button></form>'); window.submissions=0; window.keyEvents=[]; keyform.onsubmit=e=>{e.preventDefault();submissions++}; keyform.addEventListener('keydown', e=>keyEvents.push([e.key,e.isTrusted,e.code])); 'keys'")
    keyboard = reference(snapshot(), "Keyboard")
    browser("focus", keyboard)
    for key in ["Unknown", "Control+", "Ctrl+Control+a"]:
        browser("press", key, error=True)
    browser("press", "Enter")
    assert evaluate("String(submissions)") == "1", "Enter did not submit the form"
    browser("press", "Tab")
    assert evaluate("document.activeElement.id") == "keysubmit", evaluate("JSON.stringify({focus:document.activeElement.id,keyEvents})")
    browser("press", "Shift+Tab")
    assert evaluate("document.activeElement.id") == "keyfield"
    browser("type", "replace me")
    browser("press", "Meta+a" if sys.platform == "darwin" else "Control+a")
    assert evaluate("String(keyfield.selectionStart)+':'+keyfield.selectionEnd") == "0:10", evaluate("JSON.stringify({value:keyfield.value,start:keyfield.selectionStart,end:keyfield.selectionEnd,keyEvents})")
    browser("type", "replaced")
    assert evaluate("keyfield.value") == "replaced"
    assert evaluate("String(keyEvents.every(e=>e[1]))") == "true"
    evaluate("keyfield.setSelectionRange(3,3); keyfield.onkeydown=e=>{if(e.key==='a'){e.preventDefault();keyfield.onkeydown=null}}; 'cancel select all'")
    browser("press", "Meta+a" if sys.platform == "darwin" else "Control+a")
    assert evaluate("String(keyfield.selectionStart)+':'+keyfield.selectionEnd") == "3:3"
    evaluate("keyfield.addEventListener('keydown', e=>e.preventDefault(), {once:true}); 'cancel tab'")
    browser("press", "Tab")
    assert evaluate("document.activeElement.id") == "keyfield"
    browser("press", "ArrowLeft")
    assert evaluate("String(keyfield.selectionStart)") == "2"
    browser("press", "Shift+ArrowRight")
    assert evaluate("String(keyfield.selectionStart)+':'+keyfield.selectionEnd") == "2:3"
    browser("press", "Delete")
    assert evaluate("keyfield.value") == "relaced"
    print("PASS: native Enter, Tab, reverse Tab, select-all and canceled keys", flush=True)

    evaluate("document.body.insertAdjacentHTML('beforeend', '<button id=pointer>Native pointer</button>'); window.pointerEvents=[]; for(const type of ['pointerdown','pointerup','mousedown','mouseup','click','dblclick']) pointer.addEventListener(type,e=>pointerEvents.push([e.type,e.isTrusted])); 'pointer'")
    pointer = reference(snapshot(), "Native pointer")
    browser("click", pointer)
    assert json.loads(evaluate("JSON.stringify(pointerEvents)")) == [[name, True] for name in ["pointerdown", "mousedown", "pointerup", "mouseup", "click"]], "click did not use native pointer events"
    browser("hover", pointer)
    assert evaluate("String(document.querySelector('#pointer').matches(':hover'))") == "true", "hover did not activate CSS hover"
    evaluate("pointerEvents=[]; 'clear'")
    browser("dblclick", pointer)
    assert evaluate("String(pointerEvents.filter(e=>e[0]==='click').length)") == "2"
    assert evaluate("String(pointerEvents.some(e=>e[0]==='dblclick' && e[1]))") == "true"
    evaluate("document.body.insertAdjacentHTML('beforeend','<button id=moving>Moving</button>'); window.movingClicks=0; moving.onclick=()=>movingClicks++; 'moving'")
    current = snapshot()
    moving = reference(current, "Moving")
    evaluate("window.keepMoving=true; window.position=0; requestAnimationFrame(function move(){moving.style.transform='translateX('+(++position%4*20)+'px)'; if(keepMoving)requestAnimationFrame(move)}); 'animate'")
    browser("click", moving, error=True)
    evaluate("keepMoving=false; 'stop'")
    assert evaluate("String(movingClicks)") == "0", "a moving target was clicked"
    browser("click", reference(current, "Native pointer"))
    print("PASS: native click sequence, CSS hover and double-click", flush=True)


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
        marker = root / "state/flowmux/welcome-seen"
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.touch()
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
