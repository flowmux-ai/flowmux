#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise pane browser controls through the real CLI in a private GUI process."""
import base64
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
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Fixture(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/navigation":
            self.server.navigation_requests += 1
            self.server.navigation_requested.set()
            if not self.server.navigation_release.wait(10):
                self.send_error(504, "navigation fixture was not released")
                return
            body = f'<!doctype html><p id=generation>{self.server.navigation_requests}</p><input aria-label="Fresh field">'.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
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
        if self.path == "/layers":
            body = b"""<!doctype html><title>Capture layers</title><style>body{height:1200px}</style>
            <canvas id=glcanvas width=96 height=64 style="position:absolute;left:10px;top:10px"></canvas>
            <video id=clip aria-label=Clip muted playsinline src=/clip.mp4 style="position:absolute;left:150px;top:10px;width:96px;height:64px"></video>
            <script>window.gl=glcanvas.getContext('webgl',{preserveDrawingBuffer:true});
            if(gl){gl.clearColor(0,1,0,1);gl.clear(gl.COLOR_BUFFER_BIT)}</script>"""
        elif self.path == "/canvas":
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


def exercise_diagnostics(cli, url, root):
    workspace = cli("workspace", "new", "--name", "Browser diagnostics", "--root", str(root))["workspace_created"]["id"]
    terminal = next(w for w in cli("tree")["tree"]["workspaces"] if w["id"] == workspace)["panes"][0]["id"]
    pane = cli("browser", "open", url, pane_context=terminal)["browser_pane_opened"]["pane"]
    assert cli("browser", "wait", pane, "--url", url)["browser_bool_result"]["value"]
    assert cli("browser", "wait", pane, "--ready-state", "complete")["browser_bool_result"]["value"]

    def diagnose(*args, **kwargs):
        return json.loads(cli("browser", "diagnose", pane, *args, **kwargs)["browser_result"]["value"])

    report = diagnose()
    surface = report["surface_id"]
    assert report["engine"] == ("WKWebView" if sys.platform == "darwin" else "WebKitGTK"), report
    assert report["native_input"]["environment_ready"], report
    assert report["native_text"]["environment_ready"], report
    assert report["recording"]["encoder_available"], report
    assert not report["cookie_import"]["supported"] and not report["cross_origin_frames"]
    missing = diagnose(env_overrides={"PATH": str(root / "no-encoder")})
    assert missing["recording"]["reason"] == "ffmpeg_missing", missing
    cli("browser", "open", url, pane_context=terminal)
    hidden = diagnose("--surface", surface)
    assert not hidden["active"] and hidden["native_input"]["reason"] == "inactive_tab", hidden
    assert not hidden["native_input"]["environment_ready"], hidden
    cli("focus-tab", surface, "--pane", pane)
    assert diagnose()["native_input"]["environment_ready"]
    cli("browser", "eval", pane, "setTimeout(()=>confirm('diagnose'),0);'scheduled'")
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        pending = json.loads(cli("browser", "dialog", pane, "status")["browser_result"]["value"])["dialog"]
        if pending:
            break
        time.sleep(.02)
    assert pending
    try:
        assert diagnose()["native_input"]["reason"] == "dialog_pending"
    finally:
        cli("browser", "dialog", pane, "dismiss", pending["id"])
    print("BROWSER_DIAGNOSTICS_OK", flush=True)


def exercise_task_workflow(cli, url, root):
    workspace = cli("workspace", "new", "--name", "Browser task workflow", "--root", str(root))["workspace_created"]["id"]
    terminal = next(w for w in cli("tree")["tree"]["workspaces"] if w["id"] == workspace)["panes"][0]["id"]
    pane = cli("browser", "open", url, pane_context=terminal)["browser_pane_opened"]["pane"]
    assert cli("browser", "wait", pane, "--url", url)["browser_bool_result"]["value"]
    assert cli("browser", "wait", pane, "--ready-state", "complete")["browser_bool_result"]["value"]
    assert cli("browser", "wait", pane, "--selector", "#field")["browser_bool_result"]["value"]
    assert cli("browser", "wait", pane, "--js", "!!document.querySelector('#frame')?.contentDocument?.querySelector('input')")["browser_bool_result"]["value"]
    started = json.loads(cli("browser", "control", pane, "start")["browser_result"]["value"])
    task, surface = started["task_id"], started["surface_id"]

    def owned(op, *args, **kwargs):
        return cli("browser", "--task", task, "--surface", surface, op, pane, *args, **kwargs)

    def value(op, *args, **kwargs):
        return json.loads(owned(op, *args, **kwargs)["browser_result"]["value"])

    def element(target, expect):
        return json.dumps({"Element": {"target": target, "expect": expect}})

    cli("browser", "task", pane, "--name", "Unowned", error=True)
    details = value("task", "--name", "Form and files", "--step", "Fill contact details", "--waiting", "", "--user-action", "Review before submitting")
    assert details["details"]["name"] == "Form and files", details
    owned("task", "--name", "x" * 501, error=True)
    snap = value("snapshot")
    refs = {meta["name"]: ref for ref, meta in snap["refs"].items() if meta["name"] in ("Field", "Frame field", "Shadow field", "Password")}
    assert len(refs) == 4, refs
    assert owned("wait", "--condition", element(refs["Password"], {"State": "visible"}))["browser_bool_result"]["value"]
    owned("wait", "--condition", element(refs["Password"], {"Value": "secret"}), error=True)
    fields = [{"target": refs[name], "action": {"fill": text}} for name, text in [("Field", "parent"), ("Frame field", "child"), ("Shadow field", "shadow")]]
    form = value("form", json.dumps(fields))
    assert [row["status"] for row in form["fields"]] == ["verified"] * 3, form
    assert form["submit_command_sent"] is False and form["automatic_retry"] is False
    for name, text in [("Field", "parent"), ("Frame field", "child"), ("Shadow field", "shadow")]:
        assert owned("wait", "--condition", element(refs[name], {"Value": text}))["browser_bool_result"]["value"]
    assert not owned("wait", "--condition", element(refs["Frame field"], {"Value": "parent"}), "--timeout-ms", "100")["browser_bool_result"]["value"]
    changed = value("fill", refs["Shadow field"], "changed", "--receipt", "--expect", element(refs["Shadow field"], {"ValueChanged": {}}))
    assert changed["status"] == "outcome_confirmed", changed
    assert value("eval", "JSON.stringify(frame.contentWindow.submissions)") == 0
    # Reject duplicate targets before mutating any field.
    owned("form", json.dumps([fields[0], fields[0]]), error=True)
    assert owned("value", refs["Field"])["browser_result"]["value"] == "parent"
    # Per-field failure cannot hide later completion or expose password content.
    partial = value("form", json.dumps([{"target": refs["Password"], "action": {"fill": "never"}}, fields[0]]))
    assert [row["status"] for row in partial["fields"]] == ["failed", "verified"], partial
    assert "secret" not in json.dumps(partial)
    owned("eval", "contact.type='email';contact.required=true;'ok'")
    snap = value("snapshot")
    contact = next(ref for ref, meta in snap["refs"].items() if meta["name"] == "Contact email")
    invalid = value("form", json.dumps([{"target": contact, "action": {"fill": "invalid"}}]))
    assert invalid["fields"][0]["status"] == "invalid", invalid
    corrected = value("form", json.dumps([{"target": contact, "action": {"fill": "person@example.test"}}]))
    assert corrected["fields"][0]["status"] == "verified", corrected
    # Keep a current frame ref for the document replacement check.
    refs["Frame field"] = next(ref for ref, meta in snap["refs"].items() if meta["name"] == "Frame field")
    # Same CSS path in a replacement frame/document cannot satisfy an old ref.
    owned("eval", "frame.src='/frame?replaced';'ok'")
    assert owned("wait", "--js", "frame.contentWindow.location.search==='?replaced' && frame.contentDocument.readyState==='complete'")["browser_bool_result"]["value"]
    owned("wait", "--condition", element(refs["Frame field"], {"State": "visible"}), error=True)
    # Task results include real native downloads and saved captures.
    capture = root / "task-result.png"
    owned("screenshot", str(capture))
    assert capture.is_file()
    assert any(row.get("path") == str(capture) for row in value("control", "status")["results"])
    missing_capture = root / "missing-directory" / "failed.png"
    failed = value("screenshot", str(missing_capture), "--receipt")
    assert failed["error"], failed
    cards = value("control", "status")["results"]
    assert not any(row.get("path") == str(missing_capture) for row in cards), cards
    assert "Action failed" in cards[-1]["title"], cards[-1]
    value("download", "directory", str(root))
    owned("eval", "location.href='/download';'ok'")
    deadline = time.monotonic() + 10
    while True:
        downloads = value("download", "list")["downloads"]
        if downloads and downloads[-1]["state"] == "complete":
            break
        assert time.monotonic() < deadline, downloads
        time.sleep(.05)
    downloaded = downloads[-1]
    assert Path(downloaded["path"]).read_text() == "Flowmux download fixture\n"
    assert any(row.get("download_id") == downloaded["id"] and row.get("status") == "complete" and row.get("path") == downloaded["path"] for row in value("control", "status")["results"])
    # File selection remains unconfirmed until an explicit site condition is observed.
    sample = root / "upload.txt"
    sample.write_text("task upload")
    owned("eval", "upload.click();'ok'")
    deadline = time.monotonic() + 5
    while True:
        chooser = value("file-chooser", "status")["file_chooser"]
        if chooser:
            break
        assert time.monotonic() < deadline, "file chooser missing"
        time.sleep(.05)
    chosen = value("file-chooser", "select", chooser["id"], str(sample), "--root", str(root))
    assert chosen["file_chooser"] is None
    card = next(row for row in value("control", "status")["results"] if row.get("chooser_id") == chooser["id"])
    assert card["status"] == "selected" and "unconfirmed" in card["title"], card
    assert owned("wait", "--js", "uploadResult?.files[0]?.text==='task upload'")["browser_bool_result"]["value"]
    owned("eval", "document.body.insertAdjacentHTML('beforeend','<p id=uploadStatus>Waiting</p>');upload.onchange=()=>{uploadStatus.textContent='Received'};upload.value='';upload.click();'ok'")
    deadline = time.monotonic() + 5
    while True:
        chooser = value("file-chooser", "status")["file_chooser"]
        if chooser:
            break
        assert time.monotonic() < deadline
        time.sleep(.05)
    receipt = value("file-chooser", "select", chooser["id"], str(sample), "--root", str(root), "--receipt", "--expect", json.dumps({"TextChanged": {"selector": "#uploadStatus"}}))
    assert receipt["status"] == "outcome_confirmed", receipt
    card = next(row for row in value("control", "status")["results"] if row.get("chooser_id") == chooser["id"])
    assert card["status"] == "condition_observed" and "upload.txt" in card["detail"], card
    # A delayed field must stop when task ownership changes.
    owned("eval", "field.disabled=true;'ok'")
    snap = value("snapshot")
    ref = next(ref for ref, meta in snap["refs"].items() if meta["name"] == "Field")
    with ThreadPoolExecutor(max_workers=1) as workers:
        pending = workers.submit(value, "form", json.dumps([{"target": ref, "action": {"fill": "cancelled"}}]))
        deadline = time.monotonic() + 5
        while not value("control", "status")["action_in_progress"]:
            assert time.monotonic() < deadline
            time.sleep(.02)
        value("control", "handoff")
        form = pending.result(timeout=5)
        assert form["fields"][0]["status"] == "failed", form
    assert owned("value", ref)["browser_result"]["value"] == "parent"
    state = value("control", "status")
    assert state["results"] and state["state"] == "user_needed", state
    assert state["details"]["user_action"] == "Review before submitting"
    value("control", "stop")
    print("BROWSER_TASK_WORKFLOW_OK", flush=True)


def exercise_compact_snapshot(cli, url, root):
    workspace = cli("workspace", "new", "--name", "Compact observations", "--root", str(root))["workspace_created"]["id"]
    terminal = next(w for w in cli("tree")["tree"]["workspaces"] if w["id"] == workspace)["panes"][0]["id"]
    pane = cli("browser", "open", url, pane_context=terminal)["browser_pane_opened"]["pane"]
    assert cli("browser", "wait", pane, "--url", url)["browser_bool_result"]["value"]
    assert cli("browser", "wait", pane, "--ready-state", "complete")["browser_bool_result"]["value"]

    def value(op, *args):
        return cli("browser", op, pane, *args)["browser_result"]["value"]

    before = value("eval", "document.documentElement.outerHTML")
    full = json.loads(value("snapshot"))
    compact = json.loads(value("snapshot", "--compact"))
    assert "markdown" not in compact and compact["page"] == full["page"]
    assert compact["truncated"] == full["truncated"]
    assert len(compact["refs"]) == len(full["refs"])
    assert all("selector" not in meta and "states" in meta for meta in compact["refs"].values())
    assert any(meta.get("shadow_path") for meta in compact["refs"].values())
    assert any(meta.get("frame_path") for meta in compact["refs"].values())
    assert "secret" not in json.dumps(compact)
    assert value("eval", "document.documentElement.outerHTML") == before
    ref = next(k for k, v in compact["refs"].items() if v["name"] == "Contact email")
    cli("browser", "fill", pane, ref, "compact@example.test")
    assert value("value", ref) == "compact@example.test"
    restored = json.loads(value("snapshot"))
    assert "markdown" in restored and all("selector" in meta for meta in restored["refs"].values())
    cli("browser", "fill", pane, ref, "stale", error=True)
    assert value("eval", "contact.value") == "compact@example.test"
    found = json.loads(value("find", "--compact", "--role", "textbox", "--name", "Contact email", "--exact"))
    assert len(found["refs"]) == 1 and "markdown" not in found
    started = json.loads(value("control", "start"))
    task, surface = started["task_id"], started["surface_id"]
    ref = next(iter(found["refs"]))
    for index in range(25):
        receipt = json.loads(cli("browser", "--task", task, "--surface", surface,
                                 "fill", pane, ref, str(index), "--receipt")["browser_result"]["value"])
        assert receipt["status"] == "input_delivered" and receipt["side_effects_possible"]
        assert receipt["automatic_retry"] is False and receipt["error"] is None
        assert "results" not in receipt["control"], receipt
        assert receipt["control"]["task_id"] == task
    status = json.loads(value("control", "status"))
    assert len(status["results"]) == 20
    expected = {key: item for key, item in status.items() if key not in ("results", "surface_id")}
    assert receipt["control"] == expected, (receipt["control"], expected)
    assert receipt["surface_id"] == status["surface_id"]
    cli("browser", "--task", task, "--surface", surface, "control", pane, "finish")
    print("BROWSER_RECEIPT_SUMMARY_OK", flush=True)
    print("BROWSER_COMPACT_SNAPSHOT_OK", flush=True)


def exercise_ownership(cli, url, root):
    workspace = cli("workspace", "new", "--name", "Browser task", "--root", str(root))["workspace_created"]["id"]
    terminal = next(w for w in cli("tree")["tree"]["workspaces"] if w["id"] == workspace)["panes"][0]["id"]
    pane = cli("browser", "open", url, pane_context=terminal)["browser_pane_opened"]["pane"]
    assert cli("browser", "wait", pane, "--url", url)["browser_bool_result"]["value"]
    assert cli("browser", "wait", pane, "--ready-state", "complete")["browser_bool_result"]["value"]
    started = json.loads(cli("browser", "control", pane, "start")["browser_result"]["value"])
    task, surface = started["task_id"], started["surface_id"]

    def owned(op, *args, **kwargs):
        return cli("browser", "--task", task, "--surface", surface, op, pane, *args, **kwargs)

    cli("browser", "eval", pane, "window.wrong=1", error=True)
    cli("browser", "--task", "00000000-0000-0000-0000-000000000001", "eval", pane, "window.wrong=1", error=True)
    owned("eval", "document.title='Owned task';'ready'")
    deadline = time.monotonic() + 5
    while cli("browser", "title", pane)["browser_result"]["value"] != "Owned task":
        assert time.monotonic() < deadline, "native title did not update"
        time.sleep(.02)
    owned("eval", "window.submissions=0;document.body.innerHTML='<button id=submit>Submit once</button><p id=result></p>';submit.onclick=()=>{submissions++;result.textContent='Complete '+submissions};'ready'")
    snap = json.loads(owned("snapshot")["browser_result"]["value"])
    ref = next(key for key, meta in snap["refs"].items() if meta["name"] == "Submit once")
    confirmed = json.loads(owned("click", ref, "--receipt", "--expect-text", "Complete 1")["browser_result"]["value"])
    assert confirmed["status"] == "outcome_confirmed" and confirmed["outcome_met"], confirmed
    assert confirmed["surface_id"] == surface and confirmed["action_id"], confirmed
    snap = json.loads(owned("snapshot")["browser_result"]["value"])
    ref = next(key for key, meta in snap["refs"].items() if meta["name"] == "Submit once")
    unknown = json.loads(owned("click", ref, "--receipt", "--expect-text", "Never appears", "--outcome-timeout-ms", "150")["browser_result"]["value"])
    assert unknown["status"] == "outcome_unknown" and unknown["side_effects_possible"], unknown
    assert "results" not in unknown["control"]
    assert unknown["automatic_retry"] is False and unknown["action_id"] != confirmed["action_id"]
    assert owned("eval", "String(submissions)")["browser_result"]["value"] == "2", "timed-out action was repeated"
    opened = json.loads(owned("eval", "window.open('about:blank','short-lived').close();'opened once'", "--receipt")["browser_result"]["value"])
    assert any(event["kind"] == "popup" for event in opened["events"]), opened
    owned("eval", "document.body.innerHTML='<input id=input>';input.oninput=()=>window.answer=confirm('receipt?');input.focus();'ready'")
    typed = json.loads(owned("type", "x", "--receipt", "--expect-text", "Never appears")["browser_result"]["value"])
    assert typed["status"] == "dialog_pending", typed
    assert any(event["kind"] == "dialog" for event in typed["events"]), typed
    dialog = json.loads(owned("dialog", "status")["browser_result"]["value"])["dialog"]
    owned("dialog", "accept", dialog["id"])
    assert owned("wait", "--js", "answer===true")["browser_bool_result"]["value"]
    print("BROWSER_RECEIPTS_OK", flush=True)
    # A JS wait can have side effects; handoff must cancel future polling.
    with ThreadPoolExecutor(max_workers=1) as workers:
        pending = workers.submit(owned, "wait", "--js", "(window.polls=(window.polls||0)+1,false)", "--timeout-ms", "5000", "--poll-ms", "50", error=True)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            status = json.loads(owned("control", "status")["browser_result"]["value"])
            if status["action_in_progress"]:
                break
            time.sleep(.02)
        assert status["action_in_progress"]
        owned("eval", "document.title='overlap'", error=True)
        handoff = json.loads(owned("control", "handoff")["browser_result"]["value"])
        assert handoff["state"] == "user_needed"
        pending.result(timeout=5)
    owned("eval", "document.title='after handoff'", error=True)
    owned("fill", "invalid-ref", "blocked", error=True)
    assert cli("browser", "title", pane)["browser_result"]["value"] == "Owned task"
    owned("control", "stop")
    owned("eval", "document.title='after stop'", error=True)
    cli("browser", "control", pane, "start", error=True)
    assert any(tab["id"] == surface for ws in cli("tree")["tree"]["workspaces"] for p in ws["panes"] for tab in p["tabs"])
    print("BROWSER_OWNERSHIP_OK", flush=True)


def exercise_regressions(cli, url, root):
    workspace = cli("workspace", "new", "--name", "Browser regressions", "--root", str(root))["workspace_created"]["id"]
    terminal = next(w for w in cli("tree")["tree"]["workspaces"] if w["id"] == workspace)["panes"][0]["id"]
    pane = cli("browser", "open", url, pane_context=terminal)["browser_pane_opened"]["pane"]

    def browser(op, *args):
        return cli("browser", op, pane, *args)

    def evaluate(source):
        return browser("eval", source)["browser_result"]["value"]

    assert browser("wait", "--url", url)["browser_bool_result"]["value"]
    assert browser("wait", "--ready-state", "complete")["browser_bool_result"]["value"]
    evaluate("document.body.innerHTML='<input id=editing><div id=host></div>';window.clickEvents=[];const shadow=host.attachShadow({mode:'open'});shadow.innerHTML='<button>Ordered click</button>';for(const kind of ['pointerdown','pointerup','click'])shadow.querySelector('button').addEventListener(kind,e=>clickEvents.push([e.type,e.isTrusted,e.buttons]));'ready'")
    for attempt in range(8):
        evaluate("editing.focus();clickEvents=[];'ready'")
        browser("type", "x")
        snap = json.loads(browser("snapshot")["browser_result"]["value"])
        ref = next(key for key, meta in snap["refs"].items() if meta["name"] == "Ordered click")
        browser("click", ref)
        events = json.loads(evaluate("JSON.stringify(clickEvents)"))
        assert events == [["pointerdown", True, 1], ["pointerup", True, 0], ["click", True, 0]], (attempt, events)
    print("BROWSER_POINTER_ORDER_OK", flush=True)
    failures = []
    evaluate("document.body.innerHTML='<input id=f aria-label=Dialog>';f.oninput=()=>window.answer=confirm('input?');f.focus();'ready'")
    with ThreadPoolExecutor(max_workers=1) as workers:
        typed = workers.submit(browser, "type", "x")
        dialog = None
        deadline = time.monotonic() + 5
        while dialog is None and time.monotonic() < deadline:
            dialog = json.loads(browser("dialog", "status")["browser_result"]["value"])["dialog"]
            time.sleep(.02)
        assert dialog is not None, "native text did not open the dialog"
        try:
            typed.result(timeout=2)
        except TimeoutError:
            failures.append("type waited for the dialog response")
        finally:
            browser("dialog", "accept", dialog["id"])
            typed.result(timeout=5)
    assert browser("wait", "--js", "window.answer===true")["browser_bool_result"]["value"]
    assert evaluate("f.value") == "x"
    evaluate("f.oninput=null;f.focus();f.setSelectionRange(1,1);'ready'")
    try:
        browser("type", "y")
        assert evaluate("f.value") == "xy", "text cleanup blocked the next input"
    except AssertionError as error:
        failures.append(f"input after dialog: {error}; value={evaluate('f.value')}")

    evaluate("document.body.innerHTML='<div id=fixed role=button aria-label=Fixed style=\"position:fixed;left:20px;top:20px;width:80px;height:40px;background:red\"></div><div style=\"height:1800px;background:blue\"></div>';scrollTo(0,500);'ready'")
    assert browser("wait", "--js", "scrollY===500")["browser_bool_result"]["value"]
    snap = json.loads(browser("snapshot")["browser_result"]["value"])
    ref = next(key for key, meta in snap["refs"].items() if meta["name"] == "Fixed")
    path = root / "fixed.png"
    browser("screenshot", str(path), "--element", ref)
    if png_pixel(path, 10, 10) != (255, 0, 0):
        failures.append("fixed element capture saved the background")
    assert evaluate("scrollY") == "500", "capture changed scrolling"

    # Lock tests are opt-in and run only on the private Xvfb display in CI.
    if os.environ.get("FLOWMUX_TEST_X11_LOCKS") == "1":
        import ctypes
        xlib = ctypes.CDLL("libX11.so.6")
        xlib.XOpenDisplay.restype = ctypes.c_void_p
        xlib.XkbGetState.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_void_p]
        xlib.XkbLockModifiers.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_uint, ctypes.c_uint]
        xlib.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
        xlib.XCloseDisplay.argtypes = [ctypes.c_void_p]
        display = xlib.XOpenDisplay(None)
        assert display, "lock regression needs the private X11 display"

        def locked_mods():
            state = ctypes.create_string_buffer(32)
            assert xlib.XkbGetState(display, 0x100, state) == 0
            return state.raw[9]

        original = locked_mods()
        try:
            assert xlib.XkbLockModifiers(display, 0x100, 2, 2)
            xlib.XSync(display, 0)
            assert locked_mods() & 2
            evaluate("scrollTo(0,0);document.body.innerHTML='<input id=f>';f.focus();'ready'")
            for key, expected in [("a", "a"), ("A", "A"), ("Shift+a", "A"), ("1", "1"), ("Shift+1", "!")]:
                evaluate("f.value='';f.focus();'ready'")
                try:
                    browser("press", key)
                    assert evaluate("f.value") == expected
                except AssertionError as error:
                    failures.append(f"Caps Lock key {key}: {error}")
                assert locked_mods() & 2, "native input changed the user's Caps Lock state"
        finally:
            xlib.XkbLockModifiers(display, 0x100, 2, original & 2)
            xlib.XSync(display, 0)
            xlib.XCloseDisplay(display)
    assert not failures, failures
    # DOM actions must finish delivery when a modal pauses their JavaScript.
    evaluate("document.body.innerHTML='<input id=f aria-label=Dialog><input id=next aria-label=Next>';f.oninput=()=>window.answer=confirm('fill?');'ready'")
    snap = json.loads(browser("snapshot")["browser_result"]["value"])
    refs = {meta["name"]: ref for ref, meta in snap["refs"].items()}
    for operation in [("fill", refs["Dialog"], "first"), ("form", json.dumps([
            {"target": refs["Dialog"], "action": {"fill": "second"}},
            {"target": refs["Next"], "action": {"fill": "must not run"}}]))]:
        result = browser(*operation)
        pending = json.loads(browser("dialog", "status")["browser_result"]["value"])["dialog"]
        assert pending and pending["message"] == "fill?", result
        if operation[0] == "form":
            rows = json.loads(result["browser_result"]["value"])["fields"]
            assert [row["status"] for row in rows] == ["dialog_pending", "not_run"], rows
        accept = operation[0] == "form"
        browser("dialog", "accept" if accept else "dismiss", pending["id"])
        assert browser("wait", "--js", "window.answer===" + str(accept).lower())["browser_bool_result"]["value"]
        assert evaluate("next.value") == ""
    print("BROWSER_P1_REGRESSIONS_OK", flush=True)


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
    # Semantic discovery and bounded pre-input waits use the real browser, not a DOM mock.
    evaluate("document.body.innerHTML='<main aria-label=Panel><button id=go>Save</button><button>Save</button><label for=entry>이메일</label><input id=entry placeholder=주소 aria-describedby=hint disabled><span id=hint hidden>Work address only</span><input id=password type=password value=private-secret aria-label=Password><p id=result role=status>before</p></main>';window.delivered=0;go.onclick=()=>{delivered++;result.innerText=String(delivered)};'ready'")
    found = json.loads(browser("find", "--role", "button", "--name", "Save", "--exact")["browser_result"]["value"])
    assert len(found["refs"]) == 2, found
    panel = reference(snapshot(), "Panel")
    partial = json.loads(browser("snapshot", "--within", panel, "--interactive", "--max-nodes", "1")["browser_result"]["value"])
    assert len(partial["refs"]) == 1 and partial["truncated"], partial
    found = json.loads(browser("find", "--label", "이메일", "--placeholder", "주소")["browser_result"]["value"])
    entry, metadata = next(iter(found["refs"].items()))
    assert len(found["refs"]) == 1 and not metadata["states"]["enabled"], found
    assert metadata["description"] == "Work address only", metadata
    evaluate("setTimeout(()=>entry.disabled=false,250);'scheduled'")
    browser("fill", entry, "agent@example.test")
    typed = lambda condition, **kwargs: browser("wait", "--condition", json.dumps(condition), **kwargs)["browser_bool_result"]["value"]
    assert typed({"Value": {"selector": "#entry", "value": "agent@example.test"}})
    assert typed({"ElementState": {"selector": "#entry", "state": "enabled"}})
    assert typed({"ElementState": {"selector": "#entry", "state": "editable"}})
    evaluate("document.body.insertAdjacentHTML('beforeend','<input id=notText type=checkbox aria-label=Toggle><fieldset disabled><input id=disabledText aria-label=Unavailable></fieldset>');'state fixtures'")
    for selector in ("#notText", "#disabledText"):
        assert not browser("wait", "--condition", json.dumps({"ElementState": {"selector": selector, "state": "editable"}}), "--timeout-ms", "100")["browser_bool_result"]["value"]
    receipt = json.loads(browser("fill", entry, "changed", "--receipt", "--expect", json.dumps({"ValueChanged": {"selector": "#entry"}}))["browser_result"]["value"])
    assert receipt["status"] == "outcome_confirmed", receipt
    assert typed({"UrlExact": url})
    assert not browser("wait", "--condition", json.dumps({"UrlExact": url + "wrong"}), "--timeout-ms", "100")["browser_bool_result"]["value"]
    snap = snapshot()
    password = reference(snap, "Password")
    for args in [("value", password), ("attr", password, "value")]:
        rejected = browser(*args, error=True)
        assert "redacted" in str(rejected) and "private-secret" not in str(rejected), rejected
    go = next(key for key, meta in snap["refs"].items() if meta["selector"] == "#go")
    evaluate("window.cover=document.createElement('div');cover.style='position:fixed;inset:0;z-index:999999;background:white';document.body.append(cover);setTimeout(()=>cover.remove(),250);'covered'")
    receipt = json.loads(browser("click", go, "--receipt", "--expect", json.dumps({"TextChanged": {"selector": "#result"}}))["browser_result"]["value"])
    assert receipt["status"] == "outcome_confirmed" and evaluate("String(delivered)") == "1", receipt
    evaluate("go.onclick=()=>delivered++;'no visible change'")
    receipt = json.loads(browser("click", go, "--receipt", "--expect", json.dumps({"TextChanged": {"selector": "#result"}}), "--outcome-timeout-ms", "150")["browser_result"]["value"])
    assert receipt["status"] == "outcome_unknown" and evaluate("String(delivered)") == "2", receipt
    evaluate("entry.disabled=true;setTimeout(()=>{let replacement=entry.cloneNode();replacement.disabled=false;entry.replaceWith(replacement)},100);'replace while waiting'")
    entry = reference(snapshot(), "이메일")
    browser("fill", entry, "must-not-retarget", error=True)
    assert evaluate("entry.value") == "changed"
    evaluate("document.body.insertAdjacentHTML('beforeend','<button id=popupButton>Open popup</button><div style=height:5000px></div><button>Far away</button>');popupButton.onclick=()=>window.open('/popup');window.scrollTo(0,0);'popup ready'")
    full = browser("snapshot")["browser_result"]["value"]
    limited = browser("snapshot", "--viewport", "--interactive", "--max-chars", "1024")["browser_result"]["value"]
    assert "Far away" not in limited and len(limited) <= 1100, limited
    print("BROWSER_SNAPSHOT_BUDGET=" + json.dumps({"full_bytes": len(full.encode()), "partial_bytes": len(limited.encode())}), flush=True)
    popup_ref = reference(snapshot(), "Open popup")
    receipt = json.loads(browser("click", popup_ref, "--receipt", "--expect", json.dumps({"PopupReady": {"url": url + "popup"}}))["browser_result"]["value"])
    assert receipt["status"] == "outcome_confirmed", receipt
    child = next(event["identifier"] for event in receipt["events"] if event["kind"] == "popup")
    assert browser("url", "--surface", child)["browser_result"]["value"] == url + "popup"
    cli("close-tab", child, "--pane", pane)
    evaluate("document.body.insertAdjacentHTML('beforeend','<input type=range aria-label=Volume><input type=number aria-label=Quantity>');'roles'")
    for role, name in [("slider", "Volume"), ("spinbutton", "Quantity")]:
        found = json.loads(browser("find", "--role", role, "--name", name, "--exact")["browser_result"]["value"])
        assert len(found["refs"]) == 1, found
    print("BROWSER_SEMANTIC_WAIT_SECURITY_OK", flush=True)
    browser("navigate", url)
    assert browser("wait", "--url", url)["browser_bool_result"]["value"]
    assert browser("wait", "--ready-state", "complete")["browser_bool_result"]["value"]
    coordinate_parent = snapshot()["page"]["surface_id"]
    evaluate("window.recordTick=0;window.recordTimer=setInterval(()=>{document.body.style.background=++recordTick%2?'red':'blue'},150);'record ready'")
    recording = root / "browser.mp4"
    recorded = json.loads(browser("record", str(recording), "--seconds", "2", "--fps", "5")["browser_result"]["value"])
    evaluate("clearInterval(recordTimer);document.body.style.background='';'record done'")
    assert recorded["surface"] == coordinate_parent and recorded["frames"] >= 2, recorded
    probe = json.loads(subprocess.check_output(["ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json", str(recording)]))
    assert probe["streams"][0]["codec_name"] == "h264", probe
    assert 1.8 <= float(probe["format"]["duration"]) <= 3.0, probe
    decoded = subprocess.check_output(["ffmpeg", "-v", "error", "-i", str(recording), "-f", "framemd5", "-"]).decode()
    hashes = [line.rsplit(",", 1)[1].strip() for line in decoded.splitlines() if not line.startswith("#")]
    assert len(set(hashes)) > 1, "recording contained no visual changes"
    saved = recording.read_bytes()
    browser("record", str(recording), "--seconds", "1", error=True)
    assert recording.read_bytes() == saved, "recording replaced an existing file"
    resized_recording = root / "resized.mp4"
    height = int(evaluate("innerHeight"))
    with ThreadPoolExecutor(max_workers=1) as workers:
        capture = workers.submit(browser, "record", str(resized_recording), "--seconds", "3", "--fps", "5")
        time.sleep(.6)
        temporary = cli("split", pane, "--down")["pane_split_done"]["new_pane"]
        assert browser("wait", "--js", f"innerHeight < {height}")["browser_bool_result"]["value"]
        time.sleep(.8)
        cli("close-pane", temporary)
        capture.result()
    resized_duration = subprocess.check_output(["ffprobe", "-v", "error", "-show_entries", "format=duration",
                                                "-of", "default=nw=1:nk=1", str(resized_recording)])
    assert 2.8 <= float(resized_duration) <= 4.0, resized_duration
    padded = subprocess.check_output(["ffmpeg", "-v", "error", "-i", str(resized_recording),
                                      "-vf", "format=rgb24,crop=1:1:10:10", "-f", "rawvideo", "-"])
    colors = [padded[i:i+3] for i in range(0,len(padded),3)]
    assert any(max(pixel)<8 for pixel in colors) and any(max(pixel)>32 for pixel in colors), colors
    print("PASS: pinned recording retains timing and content through viewport resizing", flush=True)
    browser("navigate", url + "layers")
    assert browser("wait", "--url", url + "layers")["browser_bool_result"]["value"]
    assert browser("wait", "--js", "window.gl && clip.readyState>=2")["browser_bool_result"]["value"]
    clip = reference(snapshot(), "Clip")
    previous_video = None
    for second in (1, 2):
        browser("media", clip, "seek", str(second))
        expected = json.loads(evaluate("JSON.stringify((()=>{const c=document.createElement('canvas');c.width=96;c.height=64;const x=c.getContext('2d');x.drawImage(clip,0,0);return [...x.getImageData(24,21,1,1).data].slice(0,3)})())"))
        assert max(expected) > 32, expected
        if previous_video:
            assert sum(abs(a-b) for a,b in zip(expected,previous_video)) > 64, (previous_video,expected)
        previous_video = expected
        viewport_width, document_width = json.loads(evaluate("JSON.stringify([innerWidth,document.documentElement.scrollWidth])"))
        for mode in ((), ("--full-page",), ("--element", clip)):
            capture = root / "layers.png"
            browser("screenshot", str(capture), *mode)
            pixels = struct.unpack(">II", capture.read_bytes()[16:24])
            element = mode and mode[0] == "--element"
            width = document_width if mode == ("--full-page",) else viewport_width
            scale = pixels[0] / (96 if element else width)
            x, y = (24, 21) if element else (174, 31)
            actual = png_pixel(capture, round(x * scale), round(y * scale))
            if not all(abs(a-b) <= 8 for a,b in zip(actual,expected)):
                evidence = Path(os.environ.get("CARGO_TARGET_DIR", "target")) / "ci" / ("macos" if sys.platform == "darwin" else "linux")
                evidence.mkdir(parents=True, exist_ok=True)
                (evidence / "video-capture.png").write_bytes(capture.read_bytes())
                canvas = evaluate("(()=>{const c=document.createElement('canvas');c.width=96;c.height=64;c.getContext('2d').drawImage(clip,0,0);return c.toDataURL('image/png').split(',')[1]})()")
                (evidence / "video-canvas.png").write_bytes(base64.b64decode(canvas))
                state = json.loads(evaluate("JSON.stringify({time:clip.currentTime,paused:clip.paused,seeking:clip.seeking,ready:clip.readyState,rect:clip.getBoundingClientRect(),ratio:devicePixelRatio,p3:matchMedia('(color-gamut: p3)').matches})"))
                raise AssertionError({"mode":mode,"second":second,"actual":actual,"expected":expected,
                                      "pixels":pixels,"scale":scale,"state":state,"evidence":str(evidence)})
            if not element:
                assert png_pixel(capture, round(20 * scale), round(20 * scale)) == (0, 255, 0)
    browser("navigate", url)
    assert browser("wait", "--url", url)["browser_bool_result"]["value"]
    assert browser("wait", "--ready-state", "complete")["browser_bool_result"]["value"]
    print("PASS: viewport, document and element captures retain decoded video and WebGL pixels", flush=True)
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
    evaluate("window.slider=document.createElement('input');slider.type='range';slider.value='0';slider.style='position:fixed;left:30px;top:30px;width:240px';document.body.append(slider);window.rangeEvents=[];window.rangePointer=[];slider.oninput=e=>rangeEvents.push(e.isTrusted);for(const type of ['pointerdown','pointermove','pointerup','mousedown','mousemove','mouseup'])slider.addEventListener(type,e=>rangePointer.push([e.type,e.buttons,e.button,e.clientX,e.clientY]));'range ready'")
    bounds = json.loads(evaluate("JSON.stringify(slider.getBoundingClientRect())"))
    browser("drag", str(bounds["x"] + 8), str(bounds["y"] + bounds["height"] / 2),
            str(bounds["right"] - 12), str(bounds["y"] + bounds["height"] / 2))
    assert evaluate("String(slider.valueAsNumber>90 && rangeEvents.length>0 && rangeEvents.every(Boolean))") == "true", evaluate("JSON.stringify({value:slider.value,events:rangeEvents,pointer:rangePointer,rect:slider.getBoundingClientRect()})")
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
    assert browser("wait", "--js", "typeof mutations !== 'undefined'", "--surface", parent_surface)["browser_bool_result"]["value"]
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
    assert typed({"DownloadComplete": {"id": finished["id"]}})
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
    evaluate("document.body.insertAdjacentHTML('beforeend','<button id=dialogButton>Click dialog</button>'); dialogButton.onclick=()=>{window.dialogResult=confirm('Clicked?')}; window.blockDialogKeyUp=e=>{if(e.key==='Enter')e.stopImmediatePropagation()}; window.addEventListener('keyup',blockDialogKeyUp,true); 'ready'")
    dialog_button = reference(snapshot(), "Click dialog")
    for action in ("click", "press"):
        for accept in (True, False):
            if action == "press":
                browser("focus", dialog_button)
                browser("press", "Enter")
            else:
                browser("click", dialog_button)
            pending = dialog()
            assert pending and pending["kind"] == "confirm" and pending["message"] == "Clicked?", pending
            dialog("accept" if accept else "dismiss", pending["id"])
            assert browser("wait", "--js", f"dialogResult === {str(accept).lower()}")["browser_bool_result"]["value"]
            if action == "press":
                browser("press", "Tab")
    evaluate("window.removeEventListener('keyup',blockDialogKeyUp,true); dialogButton.remove(); 'removed'")
    evaluate("mutations.length=0; 'reset fixture changes'")
    print("PASS: page dialogs require matching IDs, complete native clicks/keys and preserve confirm/prompt/alert replies", flush=True)
    def chooser(*args, **kwargs):
        if args and args[0] == "select":
            args = (*args, "--root", str(root))
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
    browser("file-chooser", "select", pending["id"], str(upload_file), error=True)
    escape = root / "escape.txt"
    escape.symlink_to("/etc/hosts")
    chooser("select", pending["id"], str(escape), error=True)
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
    scoped = json.loads(browser("snapshot", "--within", reference(first, "Form frame"))["browser_result"]["value"])
    assert scoped["refs"] and all(meta.get("frame_path") for meta in scoped["refs"].values()), scoped
    assert any(len(meta["frame_path"]) == 2 for meta in scoped["refs"].values()), scoped
    found = json.loads(browser("find", "--label", "Shadow field", "--exact")["browser_result"]["value"])
    assert len(found["refs"]) == 1 and next(iter(found["refs"].values()))["shadow_path"], found
    first = snapshot()
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
    assert not browser("wait", "--condition", json.dumps({"Element": {"target": nested_field, "expect": {"State": "editable"}}}), "--timeout-ms", "100")["browser_bool_result"]["value"]
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
    assert not browser("wait", "--condition", json.dumps({"Element": {"target": shadow_field, "expect": {"State": "editable"}}}), "--timeout-ms", "100")["browser_bool_result"]["value"]
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
    for key in ("PageUp", "PageDown"):
        browser("press", key)
        assert json.loads(evaluate("JSON.stringify(keyEvents.at(-1).slice(0,2))")) == [key, True]
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


def exercise_navigation(cli, url, state, server):
    server.navigation_requests = 0
    server.navigation_requested = threading.Event()
    server.navigation_release = threading.Event()
    server.navigation_release.set()
    workspace = cli("workspace", "new", "--name", "Navigation completion", "--root", str(state))["workspace_created"]["id"]
    terminal = next(w for w in cli("tree")["tree"]["workspaces"] if w["id"] == workspace)["panes"][0]["id"]
    destination = url + "navigation"
    pane = cli("browser", "open", destination, pane_context=terminal)["browser_pane_opened"]["pane"]

    def browser(op, *args):
        return cli("browser", op, pane, *args)

    def evaluate(source):
        return browser("eval", source)["browser_result"]["value"]

    def wait(*args):
        return browser("wait", *args)["browser_bool_result"]["value"]

    assert wait("--condition", json.dumps({"UrlExact": destination}))
    assert wait("--ready-state", "complete")
    fresh = "typeof window.__flowmuxNavigationPending === 'undefined' && document.readyState === 'complete'"
    try:
        for generation, action in enumerate(("navigate", "reload"), 2):
            evaluate("window.__flowmuxNavigationPending=true;'marked'")
            server.navigation_requested.clear()
            server.navigation_release.clear()
            receipt = json.loads(browser(action, *([destination] if action == "navigate" else []),
                "--receipt", "--expect", json.dumps({"TextChanged": {"selector": "#generation"}}),
                "--outcome-timeout-ms", "150")["browser_result"]["value"])
            assert server.navigation_requested.wait(5), "navigation did not reach the fixture"
            assert receipt["status"] == "outcome_unknown", receipt
            assert evaluate("String(window.__flowmuxNavigationPending)") == "true"
            ready_only = wait("--ready-state", "complete", "--timeout-ms", "150")
            assert not wait("--js", fresh, "--timeout-ms", "150"), "old document satisfied the fresh-document wait"
            server.navigation_release.set()
            assert wait("--js", fresh)
            assert wait("--condition", json.dumps({"UrlExact": destination}))
            assert evaluate("document.querySelector('#generation').textContent") == str(generation)
            assert server.navigation_requests == generation, "timed-out navigation was repeated"
            snap = json.loads(browser("snapshot", "--compact")["browser_result"]["value"])
            field = next(ref for ref, meta in snap["refs"].items() if meta["name"] == "Fresh field")
            browser("fill", field, "new document")
            assert browser("value", field)["browser_result"]["value"] == "new document"
            print(f"BROWSER_NAVIGATION_COMPLETION {action}: ready_state_only={ready_only}, fresh_document_verified={generation}", flush=True)
    finally:
        server.navigation_release.set()
    print("BROWSER_NAVIGATION_COMPLETION_OK", flush=True)


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

            def cli(*args, error=False, pane_context=None, env_overrides=None):
                result = subprocess.run([str(cli_binary), "--socket", str(socket_path), "--json", *args],
                                        env={**env, **({"FLOWMUX_PANE_ID": pane_context} if pane_context else {}), **(env_overrides or {})},
                                        capture_output=True, text=True, timeout=30)
                assert bool(result.returncode) == error, (args, result.stdout, result.stderr)
                response = json.loads(result.stdout) if result.stdout else {"error": result.stderr}
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
                exercise_task_workflow(cli, f"http://127.0.0.1:{server.server_port}/", root)
                exercise_compact_snapshot(cli, f"http://127.0.0.1:{server.server_port}/", root)
                if not os.environ.get("FLOWMUX_BROWSER_TASKS_ONLY"):
                    exercise_ownership(cli, f"http://127.0.0.1:{server.server_port}/", root)
                    exercise_diagnostics(cli, f"http://127.0.0.1:{server.server_port}/", root)
                    exercise_regressions(cli, f"http://127.0.0.1:{server.server_port}/", root)
                    exercise_navigation(cli, f"http://127.0.0.1:{server.server_port}/", root, server)
                    exercise(cli, f"http://127.0.0.1:{server.server_port}/", root)
                assert process.poll() is None, "test GUI exited unexpectedly"
            finally:
                # Only the process created above is owned by this test.
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    try:
                        if sys.platform == "darwin":
                            subprocess.run(["sample", str(process.pid), "1", "1"],
                                           stdout=log, stderr=log, timeout=10)
                    finally:
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
