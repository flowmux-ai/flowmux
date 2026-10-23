#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise pane browser controls through the real CLI in a private GUI process."""
import json
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unicodedata
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Fixture(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path in ("/download", "/slow-download"):
            slow = self.path == "/slow-download"
            data = b"Flowmux download fixture\n"
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Disposition", 'attachment; filename="result.txt"')
            self.send_header("Content-Length", str(len(data) * (10000 if slow else 1)))
            self.end_headers()
            try:
                for _ in range(10000 if slow else 1):
                    self.wfile.write(data)
                    self.wfile.flush()
                    if slow:
                        time.sleep(.01)
            except (BrokenPipeError, ConnectionResetError):
                pass
            return
        if self.path == "/clip.mp4":
            data = (Path(__file__).parent / "fixtures/browser-control.mp4").read_bytes()
            start, end = 0, len(data) - 1
            partial = self.headers.get("Range", "").startswith("bytes=")
            if partial:
                first, last = self.headers["Range"][6:].split("-")
                start, end = int(first or 0), min(int(last or end), end)
            self.send_response(206 if partial else 200)
            self.send_header("Content-Type", "video/mp4")
            self.send_header("Accept-Ranges", "bytes")
            if partial:
                self.send_header("Content-Range", f"bytes {start}-{end}/{len(data)}")
            self.send_header("Content-Length", str(end - start + 1))
            self.end_headers()
            self.wfile.write(data[start:end + 1])
            return
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
        <input id=upload type=file aria-label="Upload file" accept="text/plain">
        <input id=uploads type=file multiple aria-label="Upload files">
        <iframe id=frame src=/frame title="Form frame" width=380 height=220 style="margin:20px;border:5px solid"></iframe>
        <iframe id=restricted sandbox src=/inner title="Restricted frame" width=150 height=60></iframe>
        <video id=clip controls muted playsinline preload=auto width=96 height=64
          aria-label=Clip src=/clip.mp4></video>
        <div id=shadowHost></div>
        <script>window.clicks=0;window.mutations=[];
        for (const input of [upload,uploads]) input.onchange=async event=>{
          window.uploadResult={trusted:event.isTrusted,files:await Promise.all([...input.files].map(async file=>({name:file.name,text:await file.text()})))};
        };
        const outer=shadowHost.attachShadow({mode:'open'});
        outer.innerHTML='<section id=inner></section>';
        const inner=outer.querySelector('#inner').attachShadow({mode:'open'});
        inner.innerHTML='<label for=field>Shadow field</label><input id=field><button>Shadow button</button>';
        window.shadowClicks=0;
        inner.querySelector('button').onclick=()=>shadowClicks++;
        window.shadowInputs=0;
        document.addEventListener('input',event=>{
          if(event.composedPath().includes(inner.querySelector('input'))) shadowInputs++;
        });
        for (const root of [document.documentElement,outer,inner])
          new MutationObserver(xs=>mutations.push(...xs)).observe(root,
            {subtree:true,childList:true,attributes:true,characterData:true});</script>"""
        if self.path == "/canvas":
            body = b"""<!doctype html><title>Canvas input</title>
            <style>body{height:1600px}canvas{position:absolute;left:31.5px;top:420.5px;width:240px;height:120px;background:blue}canvas:hover{outline:3px solid green}</style>
            <canvas id=canvas width=480 height=240></canvas>
            <script>window.events=[];for(const name of ['pointerdown','pointerup','click','dblclick','pointermove'])canvas.addEventListener(name,e=>events.push({type:e.type,trusted:e.isTrusted,x:e.clientX,y:e.clientY,buttons:e.buttons}));</script>"""
        elif self.path == "/popup":
            body = b"""<!doctype html><title>Popup destination</title>
            <input aria-label="Popup destination">
            <script>if(opener)opener.postMessage('navigated', location.origin)</script>"""
        elif self.path.split('?')[0] == "/frame":
            body = b"""<!doctype html><title>Frame form</title>
            <form onsubmit="event.preventDefault();window.submissions++">
              <label for=field>Frame field</label><input id=field><button>Frame submit</button>
            </form><iframe id=nested src=/inner width=300 height=120 style="border:3px solid"></iframe>
            <script>window.submissions=0</script>"""
        elif self.path == "/inner":
            body = b"""<!doctype html><title>Frame inner</title>
            <label for=field>Nested frame field</label><input id=field>
            <button style="background:blue;color:transparent;appearance:none;border:0" onclick="window.clicks++;window.lastPoint=[event.clientX,event.clientY]">Nested frame button</button>
            <script>window.clicks=0</script>"""
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


def png_pixel(path, x, y):
    png = path.read_bytes()
    width, height, depth, color, compression, filtering, interlace = struct.unpack(">IIBBBBB", png[16:29])
    assert depth == 8 and color in (2, 6) and compression == filtering == interlace == 0
    assert 0 <= x < width and 0 <= y < height
    channels = 3 if color == 2 else 4
    chunks, offset = [], 8
    while offset < len(png):
        length = int.from_bytes(png[offset:offset + 4], "big")
        if png[offset + 4:offset + 8] == b"IDAT":
            chunks.append(png[offset + 8:offset + 8 + length])
        offset += length + 12
    data = zlib.decompress(b"".join(chunks))
    stride, previous = width * channels, bytes(width * channels)
    for row in range(y + 1):
        offset = row * (stride + 1)
        method, current = data[offset], bytearray(data[offset + 1:offset + 1 + stride])
        assert method <= 4
        if method == 0:
            previous = current
            continue
        if method in (2, 4) and not any(current):
            continue
        for i in range(stride):
            left = current[i - channels] if i >= channels else 0
            up = previous[i]
            corner = previous[i - channels] if i >= channels else 0
            if method == 4:
                estimate = left + up - corner
                predictor = min((left, up, corner), key=lambda value: abs(estimate - value))
            else:
                predictor = (0, left, up, (left + up) // 2)[method]
            current[i] = (current[i] + predictor) & 255
        previous = current
    return tuple(previous[x * channels:x * channels + 3])


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
    coordinate_parent = snapshot()["page"]["surface_id"]
    cli("browser", "open", url + "canvas", pane_context=terminal)
    assert browser("wait", "--url", url + "canvas")["browser_bool_result"]["value"]
    assert browser("wait", "--ready-state", "complete")["browser_bool_result"]["value"]
    evaluate("scrollTo(0,300);'scrolled'")
    assert browser("wait", "--js", "scrollY === 300")["browser_bool_result"]["value"]
    point = json.loads(evaluate("JSON.stringify([canvas.getBoundingClientRect().x+33.5,canvas.getBoundingClientRect().y+27.5])"))
    browser("pointer", "click", *map(str, point))
    event = json.loads(evaluate("JSON.stringify(events.find(e=>e.type==='click'))"))
    assert event["trusted"] and abs(event["x"]-point[0])<1.5 and abs(event["y"]-point[1])<1.5, (event, point)
    assert json.loads(evaluate("JSON.stringify(events.filter(e=>['pointerdown','pointerup'].includes(e.type)).map(e=>[e.type,e.buttons]))")) == [["pointerdown",1],["pointerup",0]]
    event_count = evaluate("String(events.length)")
    browser("pointer", "click", "999999", "0", error=True)
    assert evaluate("String(events.length)") == event_count
    browser("pointer", "hover", *map(str, point))
    assert evaluate("String(canvas.matches(':scope:hover'))") == "true"
    browser("pointer", "dblclick", *map(str, point))
    assert evaluate("String(events.some(e=>e.type==='dblclick'&&e.trusted))") == "true"
    evaluate("events=[];window.dragged=null;canvas.onpointerdown=e=>canvas.setPointerCapture(e.pointerId);canvas.onpointermove=e=>{if(e.buttons===1)dragged=[e.clientX,e.clientY]};'drag ready'")
    destination = [point[0]+140,point[1]+45]
    browser("drag", *map(str, point + destination))
    dragged = json.loads(evaluate("JSON.stringify(dragged)"))
    assert dragged and all(abs(actual-expected)<1.5 for actual,expected in zip(dragged,destination)), (dragged,destination)
    drag_events = json.loads(evaluate("JSON.stringify(events)"))
    assert all(e["trusted"] for e in drag_events)
    assert any(e["type"] == "pointermove" and e["buttons"] == 1 for e in drag_events), drag_events
    assert [e["buttons"] for e in drag_events if e["type"] == "pointerup"] == [0], drag_events
    evaluate("events=[];'cleared'")
    browser("drag", *map(str, point + [999999,0]), error=True)
    assert evaluate("String(events.length)") == "0", "invalid drag pressed the pointer"
    evaluate("window.slider=document.createElement('input');slider.type='range';slider.value='0';slider.style='position:fixed;left:30px;top:30px;width:240px';document.body.append(slider);window.rangeEvents=[];slider.oninput=e=>rangeEvents.push(e.isTrusted);'range ready'")
    bounds = json.loads(evaluate("JSON.stringify(slider.getBoundingClientRect())"))
    browser("drag", str(bounds["x"] + 8), str(bounds["y"] + bounds["height"] / 2),
            str(bounds["right"] - 12), str(bounds["y"] + bounds["height"] / 2))
    assert evaluate("slider.valueAsNumber>90 && rangeEvents.length>0 && rangeEvents.every(Boolean)") == "true"
    evaluate("slider.remove();'range removed'")
    browser("pointer", "click", *map(str, point))
    coordinate_surface = snapshot()["page"]["surface_id"]
    cli("focus-tab", coordinate_parent, "--pane", pane)
    browser("pointer", "click", *map(str, point), "--surface", coordinate_surface, error=True)
    browser("drag", *map(str, point + destination), "--surface", coordinate_surface, error=True)
    cli("close-tab", coordinate_surface, "--pane", pane)
    assert snapshot()["page"]["surface_id"] == coordinate_parent
    print("PASS: viewport coordinates deliver trusted canvas input and reject invalid or hidden targets", flush=True)
    parent_surface = snapshot()["page"]["surface_id"]
    def wait_surface(predicate):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            current = snapshot()["page"]["surface_id"]
            if predicate(current):
                return current
            time.sleep(.02)
        raise AssertionError("popup surface did not change")

    assert evaluate("window.popupMessage=null;window.addEventListener('message',e=>{if(e.source===window.popup)window.popupMessage=e.data});window.popup=window.open('about:blank','flowmux-popup');String(!!popup)") == "true", "window.open lost its WindowProxy"
    child_surface = wait_surface(lambda current: current != parent_surface)
    assert evaluate("String(opener !== null)") == "true", "popup lost its opener"
    browser("eval", "popup.document.body.innerHTML='<input aria-label=Popup>';popup.document.title='Connected popup';'written'", "--surface", parent_surface)
    assert browser("wait", "--js", "document.title === 'Connected popup'")["browser_bool_result"]["value"]
    browser("focus", reference(snapshot(), "Popup"))
    browser("type", "한글 popup")
    assert evaluate("document.querySelector('input').value") == "한글 popup"
    assert browser("eval", "String(window.open('about:blank','flowmux-popup') === popup)", "--surface", parent_surface)["browser_result"]["value"] == "true"
    assert snapshot()["page"]["surface_id"] == child_surface
    evaluate("opener.postMessage('connected', opener.location.origin);'sent'")
    assert browser("wait", "--js", "popupMessage === 'connected'", "--surface", parent_surface)["browser_bool_result"]["value"]
    evaluate("setTimeout(()=>close(), 0);'closing'")
    assert wait_surface(lambda current: current == parent_surface) == parent_surface
    assert evaluate("String(popup.closed)") == "true"
    evaluate("window.popupMessage=null;window.popup=window.open('/popup','loaded-popup');'opening'")
    wait_surface(lambda current: current != parent_surface)
    assert browser("wait", "--url", url + "popup")["browser_bool_result"]["value"]
    assert browser("wait", "--ready-state", "complete")["browser_bool_result"]["value"]
    assert evaluate("document.title") == "Popup destination"
    assert browser("wait", "--js", "popupMessage === 'navigated'", "--surface", parent_surface)["browser_bool_result"]["value"]
    cli("close-tab", snapshot()["page"]["surface_id"], "--pane", pane)
    assert snapshot()["page"]["surface_id"] == parent_surface
    assert evaluate("popupMessage") == "navigated", "closing popup reset its opener"
    evaluate("window.open('/popup','isolated-popup','noopener');'opening'")
    wait_surface(lambda current: current != parent_surface)
    assert browser("wait", "--url", url + "popup")["browser_bool_result"]["value"]
    assert evaluate("String(opener === null)") == "true", "noopener relationship was restored"
    cli("close-tab", snapshot()["page"]["surface_id"], "--pane", pane)
    assert snapshot()["page"]["surface_id"] == parent_surface
    # A popup can close during the engine's creation handshake.
    evaluate("window.open('about:blank','short-lived').close();'closed'")
    assert browser("wait", "--js", "typeof mutations !== 'undefined'")["browser_bool_result"]["value"]
    print("PASS: native popups preserve opener, WindowProxy, input, messaging and close", flush=True)
    def downloads(*args, **kwargs):
        result = browser("download", *(args or ("list",)), **kwargs)
        return result if kwargs.get("error") else json.loads(result["browser_result"]["value"])

    folder = root / "downloads"
    folder.mkdir()
    (folder / "result.txt").write_text("keep existing file")
    initial_downloads = downloads()
    assert initial_downloads["downloads"] == []
    downloads("directory", str(folder / "missing"), error=True)
    downloads("directory", str(folder / "result.txt"), error=True)
    assert downloads()["directory"] == initial_downloads["directory"]
    assert Path(downloads("directory", str(folder))["directory"]) == folder.resolve()

    def trigger_download(path):
        evaluate(f"(()=>{{const a=document.createElement('a');a.href={json.dumps(path)};a.download='result.txt';a.click();return 'started'}})()")

    def await_download(state):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            entries = downloads()["downloads"]
            if entries and entries[-1]["state"] == state:
                return entries[-1]
            time.sleep(.05)
        raise AssertionError((state, entries))

    trigger_download("/download")
    finished = await_download("complete")
    assert Path(finished["path"]).parent == folder.resolve()
    assert Path(finished["path"]).read_bytes() == b"Flowmux download fixture\n"
    assert (folder / "result.txt").read_text() == "keep existing file"
    downloads("cancel", str(finished["id"]), error=True)
    downloads("cancel", "999999", error=True)
    trigger_download("/slow-download")
    active = await_download("in_progress")
    assert active["id"] != finished["id"]
    downloads("cancel", str(active["id"]))
    cancelled = await_download("cancelled")
    assert cancelled["id"] == active["id"]
    assert downloads()["downloads"][0]["state"] == "complete"
    evaluate("mutations.length=0; 'reset observations'")
    print("PASS: native downloads save exact content without overwrite and expose cancellation", flush=True)
    def dialog(*args, **kwargs):
        result = browser("dialog", *(args or ("status",)), **kwargs)
        return result if kwargs.get("error") else json.loads(result["browser_result"]["value"])["dialog"]

    def open_dialog(expression, kind, message):
        evaluate(f"window.dialogResult='pending'; setTimeout(()=>{{window.dialogResult={expression}}},0); 'scheduled'")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            pending = dialog()
            if pending:
                assert pending["kind"] == kind and pending["message"] == message, pending
                return pending
            time.sleep(.02)
        raise AssertionError("page dialog did not appear")

    assert dialog() is None
    pending = open_dialog("confirm('Proceed?')", "confirm", "Proceed?")
    dialog("accept", "stale-id", error=True)
    dialog("accept", pending["id"], "--text", "wrong kind", error=True)
    assert dialog()["id"] == pending["id"]
    dialog("accept", pending["id"])
    assert browser("wait", "--js", "dialogResult === true")["browser_bool_result"]["value"]
    assert dialog() is None
    next_pending = open_dialog("prompt('Name?', 'initial')", "prompt", "Name?")
    assert next_pending["default_text"] == "initial"
    dialog("dismiss", pending["id"], error=True)
    dialog("accept", next_pending["id"], "--text", "한글🙂")
    assert browser("wait", "--js", "dialogResult === '한글🙂'")["browser_bool_result"]["value"]
    pending = open_dialog("prompt('Cancel?', 'initial')", "prompt", "Cancel?")
    dialog("dismiss", pending["id"])
    assert browser("wait", "--js", "dialogResult === null")["browser_bool_result"]["value"]
    pending = open_dialog("prompt('Default?', 'initial')", "prompt", "Default?")
    dialog("accept", pending["id"])
    assert browser("wait", "--js", "dialogResult === 'initial'")["browser_bool_result"]["value"]
    pending = open_dialog("confirm('Cancel confirm?')", "confirm", "Cancel confirm?")
    dialog("dismiss", pending["id"])
    assert browser("wait", "--js", "dialogResult === false")["browser_bool_result"]["value"]
    pending = open_dialog("alert('Done')", "alert", "Done")
    dialog("dismiss", pending["id"])
    assert browser("wait", "--js", "dialogResult === undefined")["browser_bool_result"]["value"]
    assert dialog() is None
    print("PASS: page dialogs require matching IDs and preserve confirm/prompt/alert replies", flush=True)
    def chooser(*args, **kwargs):
        result = browser("file-chooser", *(args or ("status",)), **kwargs)
        return result if kwargs.get("error") else json.loads(result["browser_result"]["value"])["file_chooser"]

    def open_chooser(name):
        browser("focus", reference(snapshot(), name))
        browser("press", "Enter")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            pending = chooser()
            if pending:
                return pending
            time.sleep(.02)
        raise AssertionError("file chooser did not appear")

    upload_file = root / "한글 upload.txt"
    upload_file.write_text("private fixture 한글🙂")
    another_file = root / "second.txt"
    another_file.write_text("second fixture")
    assert chooser() is None
    pending = open_chooser("Upload file")
    assert not pending["multiple"]
    chooser("select", "stale-id", str(upload_file), error=True)
    chooser("select", pending["id"], str(root / "missing"), error=True)
    chooser("select", pending["id"], str(root), error=True)
    chooser("select", pending["id"], str(upload_file), str(another_file), error=True)
    assert chooser()["id"] == pending["id"]
    chooser("select", pending["id"], str(upload_file))
    assert browser("wait", "--js", "window.uploadResult?.files.length === 1")["browser_bool_result"]["value"]
    uploaded = json.loads(evaluate("JSON.stringify(uploadResult)"))
    for item in uploaded["files"]:
        item["name"] = unicodedata.normalize("NFC", item["name"])
    assert uploaded == {"trusted": True, "files": [{"name": upload_file.name, "text": upload_file.read_text()}]}, uploaded
    pending = open_chooser("Upload files")
    assert pending["multiple"]
    chooser("select", pending["id"], str(upload_file), str(another_file))
    assert browser("wait", "--js", "window.uploadResult?.files.length === 2")["browser_bool_result"]["value"]
    uploaded = json.loads(evaluate("JSON.stringify(uploadResult)"))
    for item in uploaded["files"]:
        item["name"] = unicodedata.normalize("NFC", item["name"])
    assert uploaded["trusted"] and uploaded["files"] == [{"name": f.name, "text": f.read_text()} for f in [upload_file, another_file]], uploaded
    previous_id = pending["id"]
    pending = open_chooser("Upload files")
    chooser("cancel", previous_id, error=True)
    chooser("cancel", pending["id"])
    assert chooser() is None
    assert evaluate("String(uploads.files.length)") == "2", "cancel replaced existing files"
    original_surface = snapshot()["page"]["surface_id"]
    cli("browser", "open", url, pane_context=terminal)
    assert browser("wait", "--url", url)["browser_bool_result"]["value"]
    assert browser("wait", "--ready-state", "complete")["browser_bool_result"]["value"]
    closing_surface = snapshot()["page"]["surface_id"]
    pending = open_chooser("Upload file")
    cli("close-tab", closing_surface, "--pane", pane)
    assert snapshot()["page"]["surface_id"] == original_surface
    chooser("select", pending["id"], str(upload_file), error=True)
    assert chooser() is None
    print("PASS: native file selection validates paths and IDs and delivers exact file contents", flush=True)
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
    evaluate("window.captureBox=document.createElement('div'); captureBox.style='width:700px;height:1800px;background:red'; document.body.append(captureBox); window.scrollTo(40,100); 'capture fixture'")
    size = json.loads(evaluate("JSON.stringify({width:document.documentElement.scrollWidth,height:document.documentElement.scrollHeight,x:scrollX,y:scrollY})"))
    viewport_path, full_path = root / "viewport.png", root / "full.png"
    browser("screenshot", str(viewport_path))
    browser("screenshot", str(full_path), "--full-page")
    viewport_png, full_png = viewport_path.read_bytes(), full_path.read_bytes()
    assert full_png[:8] == viewport_png[:8] == b"\x89PNG\r\n\x1a\n"
    viewport_width, viewport_height = struct.unpack(">II", viewport_png[16:24])
    width, height = struct.unpack(">II", full_png[16:24])
    assert height > viewport_height
    assert abs(width / size["width"] - height / size["height"]) < .01, (width, height, size)
    assert png_pixel(full_path, width // 2, height - round(20 * width / size["width"])) == (255, 0, 0)
    assert evaluate("JSON.stringify([scrollX,scrollY])") == json.dumps([size["x"], size["y"]], separators=(",", ":"))
    evaluate("captureBox.style.height='10000000px'; 'oversized document'")
    browser("screenshot", str(root / "oversized.png"), "--full-page", error=True)
    assert not (root / "oversized.png").exists()
    evaluate("captureBox.remove(); window.scrollTo(0,0); 'capture fixture removed'")
    print("PASS: full-document PNG covers offscreen content and preserves scrolling", flush=True)
    def capture_element(ref, rect, scale=1):
        path = root / "element.png"
        browser("screenshot", str(path), "--element", ref)
        width, height = struct.unpack(">II", path.read_bytes()[16:24])
        device_scale = viewport_width / float(evaluate("String(innerWidth)"))
        assert abs(width - rect["width"] * scale * device_scale) <= 2, (width, rect, scale)
        assert abs(height - rect["height"] * scale * device_scale) <= 2, (height, rect, scale)
        return path, width, height

    evaluate("target.disabled=true; target.inert=true; 'disabled capture target'")
    capture_element(reference(first, "Original"), json.loads(evaluate("JSON.stringify(target.getBoundingClientRect())")))
    evaluate("target.disabled=false; target.inert=false; 'restored capture target'")
    capture_element(reference(first, "Shadow button"), json.loads(evaluate("JSON.stringify(shadowHost.shadowRoot.querySelector('#inner').shadowRoot.querySelector('button').getBoundingClientRect())")))
    browser("screenshot", str(root / "invalid.png"), "--element", "e999-invalid", error=True)
    assert not (root / "invalid.png").exists()
    print("PASS: element capture includes disabled and shadow targets and rejects unknown refs", flush=True)
    frame_field = reference(first, "Frame field")
    nested_field = reference(first, "Nested frame field")
    assert [entry["selector"] for entry in first["refs"][nested_field]["frame_path"]] == ["#frame", "#nested"]
    assert not first["refs"][reference(first, "Restricted frame")]["frame_accessible"]
    browser("click", reference(first, "Restricted frame"), error=True)
    browser("fill", frame_field, "frame")
    browser("focus", frame_field)
    browser("type", " input")
    browser("press", "Backspace")
    assert browser("value", frame_field)["browser_result"]["value"] == "frame inpu"
    browser("press", "Enter")
    assert evaluate("String(frame.contentWindow.submissions)") == "1"
    browser("press", "Tab")
    browser("press", "Tab")
    assert evaluate("frame.contentDocument.querySelector('#nested').contentDocument.activeElement.id") == "field"
    browser("press", "Shift+Tab")
    assert evaluate("frame.contentDocument.activeElement.tagName") == "BUTTON"
    browser("fill", nested_field, "nested")
    assert browser("value", nested_field)["browser_result"]["value"] == "nested"
    assert browser("value", reference(first, "Field"))["browser_result"]["value"] == ""
    print("PASS: nested iframe names and native input stay in the intended document", flush=True)
    nested_button = reference(first, "Nested frame button")
    evaluate("frame.style.padding='20px'; frame.style.width='380.5px'; 'frame box'")
    for scale in [1, .8, 1.25]:
        evaluate(f"frame.style.transform='scale({scale})'; frame.style.transformOrigin='0 0'; 'scale'")
        path, width, height = capture_element(nested_button, json.loads(evaluate("JSON.stringify(frame.contentDocument.querySelector('#nested').contentDocument.querySelector('button').getBoundingClientRect())")), scale)
        assert png_pixel(path, width // 2, height // 8) == (0, 0, 255), scale
        assert png_pixel(path, width // 2, height * 7 // 8) == (0, 0, 255), scale
    print("PASS: nested frame element captures account for padding and scale", flush=True)
    for index, scale in enumerate([1, .8, 1.25], 1):
        evaluate(f"frame.style.transform='scale({scale})'; frame.style.transformOrigin='0 0'; 'scale'")
        browser("click", nested_button)
        assert evaluate("String(frame.contentDocument.querySelector('#nested').contentWindow.clicks)") == str(index)
        assert evaluate("String((()=>{const w=frame.contentDocument.querySelector('#nested').contentWindow;const r=w.document.querySelector('button').getBoundingClientRect();return Math.abs(w.lastPoint[0]-(r.left+r.width/2))<1.5 && Math.abs(w.lastPoint[1]-(r.top+r.height/2))<1.5})())") == "true", evaluate("JSON.stringify({point:frame.contentDocument.querySelector('#nested').contentWindow.lastPoint,button:frame.contentDocument.querySelector('#nested').contentDocument.querySelector('button').getBoundingClientRect(),frame:frame.getBoundingClientRect(),nested:frame.contentDocument.querySelector('#nested').getBoundingClientRect(),scale:getComputedStyle(frame).transform})")
    evaluate("frame.style.transform='rotate(10deg)'; 'rotate'")
    browser("click", nested_button, error=True)
    browser("screenshot", str(root / "rotated.png"), "--element", nested_button, error=True)
    evaluate("frame.style.transform=''; frame.style.opacity='0'; 'hidden frame'")
    assert not browser("is-visible", nested_field)["browser_bool_result"]["value"]
    browser("fill", nested_field, "hidden", error=True)
    evaluate("frame.style.opacity='1'; frame.inert=true; 'inert frame'")
    assert not browser("is-enabled", nested_field)["browser_bool_result"]["value"]
    browser("fill", nested_field, "inert", error=True)
    evaluate("frame.inert=false; window.frameCover=document.createElement('div'); frameCover.style='position:fixed;inset:0;z-index:99999'; document.body.append(frameCover); 'covered frame'")
    browser("click", nested_button, error=True)
    evaluate("frameCover.remove(); 'uncovered frame'")
    assert evaluate("String(frame.contentDocument.querySelector('#nested').contentWindow.clicks)") == "3"
    evaluate("frame.src='/frame?reloaded'; 'navigate frame'")
    assert browser("wait", "--js", "frame.contentDocument?.title === 'Frame form' && frame.contentWindow.submissions === 0 && frame.contentDocument.querySelector('#nested')?.contentDocument?.title === 'Frame inner'")["browser_bool_result"]["value"]
    browser("fill", frame_field, "stale", error=True)
    browser("screenshot", str(root / "stale.png"), "--element", nested_button, error=True)
    browser("fill", nested_field, "stale", error=True)
    first = snapshot()
    browser("fill", reference(first, "Frame field"), "fresh")
    assert browser("value", reference(first, "Frame field"))["browser_result"]["value"] == "fresh"
    print("PASS: native iframe clicks and child-navigation ref invalidation", flush=True)
    shadow_field = reference(first, "Shadow field")
    assert first["refs"][shadow_field]["shadow_path"] == ["#shadowHost", "#inner"]
    browser("fill", shadow_field, "inside")
    assert browser("value", shadow_field)["browser_result"]["value"] == "inside"
    assert evaluate("String(shadowInputs)") == "1", "fill did not reach the outer application"
    assert browser("value", reference(first, "Field"))["browser_result"]["value"] == ""
    browser("focus", shadow_field)
    browser("type", " shadow")
    assert browser("value", shadow_field)["browser_result"]["value"] == "inside shadow"
    browser("press", "Backspace")
    assert browser("value", shadow_field)["browser_result"]["value"] == "inside shado"
    evaluate("shadowHost.inert=true; 'inert host'")
    assert not browser("is-enabled", shadow_field)["browser_bool_result"]["value"]
    browser("fill", shadow_field, "blocked", error=True)
    evaluate("shadowHost.inert=false; shadowHost.style.opacity='0'; 'hidden host'")
    assert not browser("is-visible", shadow_field)["browser_bool_result"]["value"]
    browser("fill", shadow_field, "blocked", error=True)
    evaluate("shadowHost.style.opacity='1'; 'visible host'")
    assert browser("value", shadow_field)["browser_result"]["value"] == "inside shado"
    print("PASS: nested Shadow DOM names, fill, focus and native text input", flush=True)
    evaluate("window.shadowCover=document.createElement('div'); shadowCover.style='position:fixed;inset:0;z-index:999999;background:white'; document.body.append(shadowCover); 'covered'")
    browser("click", reference(first, "Shadow button"), error=True)
    assert evaluate("String(shadowClicks)") == "0"
    evaluate("shadowCover.remove(); 'uncovered'")
    try:
        browser("click", reference(first, "Shadow button"))
    except AssertionError:
        print("SHADOW_POINTER_FAILURE_STATE=" + evaluate("JSON.stringify({rect:shadowHost.shadowRoot.querySelector('#inner').shadowRoot.querySelector('button').getBoundingClientRect(),viewport:[innerWidth,innerHeight],scroll:[scrollX,scrollY]})"), flush=True)
        raise
    assert evaluate("String(shadowClicks)") == "1"
    evaluate("shadowHost.outerHTML=shadowHost.outerHTML; 'replaced shadow host'")
    browser("fill", shadow_field, "stale", error=True)
    print("PASS: nested open Shadow DOM refs target their own roots and expire with their host", flush=True)
    clip = reference(first, "Clip")

    def media(action, *args):
        return json.loads(browser("media", clip, action, *args)["browser_result"]["value"])

    assert browser("wait", "--js", "clip.readyState >= 2")["browser_bool_result"]["value"], media("status")
    initial = media("status")
    assert initial["paused"] and initial["muted"] and 3.9 <= initial["duration"] <= 4.1, initial
    playing = media("play")
    assert not playing["paused"] and playing["current_time"] > 0, playing
    assert evaluate("String(clip.videoWidth) + 'x' + clip.videoHeight") == "96x64"
    paused = media("pause")
    assert paused["paused"], paused
    sought = media("seek", "2")
    assert not sought["seeking"] and abs(sought["current_time"] - 2) < .1, sought
    assert not media("unmute")["muted"]
    assert media("mute")["muted"]
    browser("media", clip, "seek", "100", error=True)
    assert abs(media("status")["current_time"] - 2) < .1
    browser("media", reference(first, "Field"), "play", error=True)
    evaluate("clip.src='/broken'; clip.load(); 'invalid media'")
    assert browser("wait", "--js", "clip.error !== null")["browser_bool_result"]["value"]
    assert media("status")["error"]["code"] in (3, 4)
    browser("media", clip, "play", error=True)
    evaluate("clip.outerHTML=clip.outerHTML; 'replaced media element'")
    browser("media", clip, "play", error=True)
    assert evaluate("String(clip.paused)") == "true"
    print("PASS: media playback advances, pause/seek/mute report actual state", flush=True)
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
    assert downloads()["downloads"] == []
    assert downloads()["directory"] == initial_downloads["directory"]
    pinned = downloads("list", "--surface", first_surface)
    assert [item["state"] for item in pinned["downloads"]] == ["complete", "cancelled"]
    assert Path(pinned["directory"]) == folder.resolve()
    browser("click", reference(third, "Original"), error=True)
    assert evaluate("String(clicks)") == "0", "old tab ref reached new tab"
    browser("click", reference(other, "Original"))
    assert evaluate("String(clicks)") == "1"
    print("PASS: stale nodes, snapshots and tabs rejected; live actions and non-mutating snapshots preserved", flush=True)
    assert first_surface, "snapshot must identify its browser tab"
    browser("eval", "setTimeout(()=>{window.pinnedDialog=confirm('Pinned tab?')},0); 'scheduled'", "--surface", first_surface)
    deadline = time.monotonic() + 5
    pending = None
    while not pending and time.monotonic() < deadline:
        pending = dialog("status", "--surface", first_surface)
        time.sleep(.02)
    assert pending and pending["message"] == "Pinned tab?", pending
    assert dialog() is None, "inactive tab dialog leaked into active tab state"
    dialog("accept", pending["id"], error=True)
    dialog("accept", pending["id"], "--surface", first_surface)
    assert browser("wait", "--js", "pinnedDialog === true", "--surface", first_surface)["browser_bool_result"]["value"]
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
    try:
        browser("hover", button)
    except AssertionError:
        print("HOVER_FAILURE_STATE=" + evaluate("JSON.stringify({hover:target.matches(':hover'),disabled:target.disabled,focus:document.hasFocus(),rect:target.getBoundingClientRect().toJSON(),viewport:[innerWidth,innerHeight]})"), flush=True)
        raise
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

    evaluate("document.body.insertAdjacentHTML('beforeend', '<button id=pointer>Native pointer</button>'); window.pointerEvents=[]; for(const type of ['pointerdown','pointerup','mousedown','mouseup','click','dblclick']) pointer.addEventListener(type,e=>pointerEvents.push([e.type,e.isTrusted,e.buttons])); 'pointer'")
    pointer = reference(snapshot(), "Native pointer")
    browser("click", pointer)
    assert json.loads(evaluate("JSON.stringify(pointerEvents)")) == [[name, True, 1 if name in ("pointerdown", "mousedown") else 0] for name in ["pointerdown", "mousedown", "pointerup", "mouseup", "click"]], "click did not use native pointer events"
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
