#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify login import using a private Flowmux, keyring and synthetic Chrome DB.

Uses the existing Xvfb/D-Bus/XDG harness. Requires GI/Atspi, python-xlib,
cryptography, GNOME Keyring and Pillow. Never reads real login cookies.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import threading
import time
import tomllib
from http.server import BaseHTTPRequestHandler, HTTPServer

from cryptography.hazmat.primitives import padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "gui", Path(__file__).with_name("test-ssh-workspace-gui.py"))
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--gui", default="target/debug/flowmux")
parser.add_argument("--cli", default="target/debug/flowmuxctl")
parser.add_argument("--protected-pid", type=int, action="append", default=[])
args = parser.parse_args()
for key in ("gui", "cli"):
    setattr(args, key, str(Path(getattr(args, key)).resolve(strict=True)))
h = gui.Harness(args)
h.env.update(GTK_A11Y="atspi", GTK_USE_PORTAL="0")
server = None
writer = None

try:
    config = h.root / "config/flowmux"
    config.mkdir()
    clean = h.root / "clean-shell"
    clean.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
    clean.chmod(0o755)
    (config / "options.json").write_text(json.dumps({
        "default_shell": str(clean), "system_notifications_enabled": False,
        "persist_browser_session": True,
    }))
    state = h.root / "state/flowmux"
    state.mkdir()
    manifest = tomllib.loads(Path(__file__).resolve().parents[1].joinpath("Cargo.toml").read_text())
    (state / "installed-skill-version").write_text(manifest["workspace"]["package"]["version"])
    browser = h.root / "config/google-chrome/Default/Network"
    browser.mkdir(parents=True)
    writer = sqlite3.connect(browser / "Cookies")
    writer.executescript("""
        PRAGMA journal_mode=WAL;
        CREATE TABLE meta(key TEXT, value TEXT);
        INSERT INTO meta VALUES('version','24');
        CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,path TEXT,
            expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,
            samesite INTEGER,encrypted_value BLOB,top_frame_site_key TEXT);
    """)
    fixture_password = b"flowmux-fixture-key"
    key = hashlib.pbkdf2_hmac("sha1", fixture_password, b"saltysalt", 1, 16)

    def encrypted(host, value):
        padder = padding.PKCS7(128).padder()
        padded = padder.update(hashlib.sha256(host.encode()).digest() + value.encode()) + padder.finalize()
        cipher = Cipher(algorithms.AES(key), modes.CBC(b" " * 16)).encryptor()
        return b"v11" + cipher.update(padded) + cipher.finalize()

    expiry = (int(time.time()) + 3600 + 11644473600) * 1000000
    for host, name, value, partition in [
        ("127.0.0.1", "sid", "fixture-session", ""),
        ("127.0.0.1", "partitioned", "private-partition", "https://other.test"),
        ("unrelated.test", "unrelated", "private-other", ""),
    ]:
        writer.execute("INSERT INTO cookies VALUES(?,?,'','/',?,0,1,1,?,?)",
                       (host, name, expiry, encrypted(host, value), partition))
    writer.commit()
    original = list(writer.execute("SELECT * FROM cookies"))

    h.start_display()
    os.environ["DISPLAY"] = h.env["DISPLAY"]
    os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
    os.environ.pop("AT_SPI_BUS_ADDRESS", None)
    import gi
    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi, Gio, GLib
    Atspi.set_timeout(1500, 1500)
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(path, interface, method, signature, values):
        return bus.call_sync("org.freedesktop.secrets", path, interface, method,
                             GLib.Variant(signature, values), None,
                             Gio.DBusCallFlags.NO_AUTO_START, 5000, None).unpack()

    with (h.root / "keyring.log").open("w") as log:
        keyring = h.spawn(["gnome-keyring-daemon", "--foreground", "--unlock",
                           "--components=secrets", "--control-directory", str(h.root / "keyring-control")],
                          stdin=subprocess.PIPE, stdout=log, stderr=log)
    keyring.stdin.write(b"fixture-keyring-password")
    keyring.stdin.close()

    def keyring_ready():
        try:
            return call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service",
                        "ReadAlias", "(s)", ("default",))[0] != "/"
        except GLib.Error:
            return False

    gui.wait_for(keyring_ready, "private keyring")
    def install_fixture_key():
        session = call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service",
                       "OpenSession", "(sv)", ("plain", GLib.Variant("s", "")))[1]
        collection = call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service",
                          "ReadAlias", "(s)", ("default",))[0]
        call(collection, "org.freedesktop.Secret.Collection", "CreateItem", "(a{sv}(oayays)b)", ({
            "org.freedesktop.Secret.Item.Label": GLib.Variant("s", "Chrome Safe Storage fixture"),
            "org.freedesktop.Secret.Item.Attributes": GLib.Variant("a{ss}", {"application": "chrome"}),
        }, (session, [], list(fixture_password), "text/plain"), True))
        call(session, "org.freedesktop.Secret.Session", "Close", "()", ())

    class Page(BaseHTTPRequestHandler):
        def do_GET(self):
            signed_in = "sid=fixture-session" in self.headers.get("Cookie", "")
            title = "Signed in" if signed_in else "Sign in"
            body = f"<!doctype html><title>{title}</title><h1>{title}</h1>".encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    server = HTTPServer(("127.0.0.1", 0), Page)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_port}/"
    process, sock = h.window("import-first")
    h.rpc(sock, "workspace_create", name="Login import fixture", root=str(h.root), team=False)

    def cli(*arguments, source=None):
        command_env = dict(h.env)
        if source:
            command_env["FLOWMUX_PANE_ID"] = source
        result = subprocess.run([args.cli, "--socket", str(sock), *arguments],
                                env=command_env, capture_output=True, text=True, timeout=30)
        assert result.returncode == 0, f"{arguments}: {result.stderr} {result.stdout}"
        return result.stdout.strip()

    def open_page():
        workspace = gui.wait_for(lambda: next(iter(h.tree(sock)), None), "workspace restored")
        source = workspace["panes"][0]["id"]
        opened = json.loads(cli("--json", "browser", "open", url, source=source))
        pane = opened["browser_pane_opened"]["pane"]
        assert cli("browser", "wait", pane, "--ready-state", "complete") == "true"
        return pane

    def walk(node):
        node.clear_cache()
        yield node
        for index in range(node.get_child_count()):
            child = node.get_child_at_index(index)
            if child:
                yield from walk(child)

    def find(name, actionable=False):
        def lookup():
            desktop = Atspi.get_desktop(0)
            desktop.clear_cache()
            for index in range(desktop.get_child_count()):
                app = desktop.get_child_at_index(index)
                if app.get_process_id() == process.pid:
                    return next((node for node in walk(app) if node.get_name() == name
                                 and node.get_state_set().contains(Atspi.StateType.SHOWING)
                                 and (not actionable or node.get_state_set().contains(Atspi.StateType.SENSITIVE))), None)
            return None
        return gui.wait_for(lookup, name, timeout=15)

    def click(name):
        action = find(name, actionable=True).get_action_iface()
        assert action and action.get_n_actions() and action.do_action(0), name

    pane = open_page()
    gui.wait_for(lambda: cli("browser", "title", pane) == "Sign in", "initial signed-out page")
    click("Import cookies")
    find("Import cookies?")
    from PIL import ImageGrab
    ImageGrab.grab(xdisplay=h.env["DISPLAY"]).save(h.root / "import-confirm.png")
    click("Cancel")
    assert cli("browser", "title", pane) == "Sign in"
    h.pass_check("cancel leaves the destination signed out")
    click("Import cookies")
    click("Import")
    find("Cookie import failed")
    click("Close")
    assert cli("browser", "title", pane) == "Sign in"
    h.pass_check("missing keyring key reports an error without changing login state")
    install_fixture_key()
    click("Import cookies")
    click("Import")
    find("Cookies imported")
    ImageGrab.grab(xdisplay=h.env["DISPLAY"]).save(h.root / "import-result.png")
    click("Close")
    gui.wait_for(lambda: cli("browser", "title", pane) == "Signed in", "cookie-backed sign-in")
    assert cli("browser", "eval", pane, "document.cookie") in ("", '""')
    h.pass_check("v11 keyring decryption and HttpOnly cookie insertion sign in on reload")
    assert list(writer.execute("SELECT * FROM cookies")) == original
    h.pass_check("source database remains unchanged while open in WAL mode")
    other_pane = open_page()
    gui.wait_for(lambda: cli("browser", "title", other_pane) == "Signed in", "shared profile login")
    h.pass_check("new browser tabs share the imported login")
    h.stop(process)
    process, sock = h.window("import-restarted")
    pane = open_page()
    gui.wait_for(lambda: cli("browser", "title", pane) == "Signed in", "persisted login after restart")
    h.pass_check("imported persistent cookie survives isolated Flowmux restart")
    h.stop(process)
    print("BROWSER_IMPORT_GUI_OK", flush=True)
finally:
    if server:
        server.shutdown()
    if writer:
        writer.close()
    h.cleanup()
