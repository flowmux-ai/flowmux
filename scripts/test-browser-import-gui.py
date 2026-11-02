#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify login import using a private Flowmux, keyring and synthetic Chrome DB.

Uses the existing Xvfb/D-Bus/XDG harness. Requires GI/Atspi, python-xlib,
cryptography, GNOME Keyring, gcr-prompter and Pillow. Includes the 120-second
approval timeout. Never reads real login cookies.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
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
# Isolate home-based Firefox/Snap profiles as well as XDG-based Chromium profiles.
fixture_home = h.root / "home"
fixture_home.mkdir()
h.env["HOME"] = str(fixture_home)
h.env.pop("AT_SPI_BUS_ADDRESS", None)
h.env.update(GTK_A11Y="atspi", GTK_USE_PORTAL="0", LANG="C.UTF-8", LC_ALL="C.UTF-8")
server = None
writer = None

try:
    config = h.root / "config/flowmux"
    config.mkdir()
    clean = h.root / "clean-shell"
    clean.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
    clean.chmod(0o755)
    options = {
        "default_shell": str(clean), "system_notifications_enabled": False,
        "persist_browser_session": True,
    }
    (config / "options.json").write_text(json.dumps(options))
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
        # Keep Flowmux's fatal checks; GNOME Keyring logs criticals on late replies to expired prompts.
        keyring = h.spawn(["env", "-u", "G_DEBUG", "gnome-keyring-daemon", "--foreground", "--unlock",
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
        assert cli("browser", "wait", pane, "--url", url) == "true"
        assert cli("browser", "wait", pane, "--ready-state", "complete") == "true"
        gui.wait_for(lambda: cli("browser", "title", pane) in ("Sign in", "Signed in"), "fixture document loaded")
        return pane

    def walk(node):
        node.clear_cache()
        yield node
        for index in range(node.get_child_count()):
            child = node.get_child_at_index(index)
            if child:
                yield from walk(child)

    def lookup(name, actionable=False, showing=True, role=None, last=False):
        desktop = Atspi.get_desktop(0)
        desktop.clear_cache()
        for index in range(desktop.get_child_count()):
            app = desktop.get_child_at_index(index)
            if app.get_process_id() == process.pid:
                matches = [node for node in walk(app) if node.get_name() == name
                           and (role is None or node.get_role() == role)
                           and (not showing or node.get_state_set().contains(Atspi.StateType.SHOWING))
                           and (not actionable or node.get_state_set().contains(Atspi.StateType.SENSITIVE))]
                return matches[-1 if last else 0] if matches else None
        return None

    def find(name, actionable=False):
        return gui.wait_for(lambda: lookup(name, actionable), name, timeout=15)

    def click(name):
        button = gui.wait_for(lambda: lookup(name, actionable=True, role=Atspi.Role.PUSH_BUTTON, last=True), name)
        action = button.get_action_iface()
        assert action and action.get_n_actions() and action.do_action(0), name

    def press(key):
        from Xlib import X, XK, display
        from Xlib.ext import xtest
        connection = display.Display(h.env["DISPLAY"])
        try:
            code = connection.keysym_to_keycode(XK.string_to_keysym(key))
            xtest.fake_input(connection, X.KeyPress, code)
            xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()
        finally:
            connection.close()

    def close_request():
        from Xlib import X, display, protocol
        connection = display.Display(h.env["DISPLAY"])
        try:
            for window in connection.screen().root.query_tree().children:
                pid = window.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                if pid is not None and int(pid.value[0]) == process.pid:
                    window.send_event(protocol.event.ClientMessage(
                        window=window, client_type=connection.intern_atom("WM_PROTOCOLS"),
                        data=(32, [connection.intern_atom("WM_DELETE_WINDOW"), X.CurrentTime, 0, 0, 0])))
            connection.sync()
        finally:
            connection.close()

    def navigate(target):
        cli("browser", "navigate", pane, target)
        assert cli("browser", "wait", pane, "--url", target) == "true"
        assert cli("browser", "wait", pane, "--ready-state", "complete") == "true"

    def assert_idle():
        gui.wait_for(lambda: not lookup("Import cookies?", showing=False)
                     and not lookup("Importing cookies…", showing=False),
                     "no confirmation or progress dialog remains", timeout=5)
        find("Import cookies", actionable=True)
        assert process.poll() is None

    def dismiss_result(title, escape=False):
        find(title)
        assert lookup(title, role=Atspi.Role.LABEL), f"missing dialog heading: {title}"
        press("Escape") if escape else click("Close")
        gui.wait_for(lambda: not lookup(title, showing=False, role=Atspi.Role.LABEL)
                     and not lookup("Importing cookies…", showing=False),
                     f"{title}: result and progress dialogs dismissed", timeout=5)
        assert_idle()
        h.pass_check(f"{title}: no progress dialog remains after Close")

    def import_result(title, body=None):
        click("Import cookies")
        click("Import")
        find(title)
        if body:
            find(body)
        dismiss_result(title)

    def assert_signed_out():
        cli("browser", "reload", pane)
        assert cli("browser", "wait", pane, "--ready-state", "complete") == "true"
        gui.wait_for(lambda: cli("browser", "title", pane) == "Sign in", "still signed out")

    def restore_rows():
        writer.execute("DELETE FROM cookies")
        writer.executemany("INSERT INTO cookies VALUES(?,?,?,?,?,?,?,?,?,?)", original)
        writer.commit()

    pane = open_page()
    gui.wait_for(lambda: cli("browser", "title", pane) == "Sign in", "initial signed-out page")
    for target in ("about:blank", (h.root / "empty.html").as_uri()):
        (h.root / "empty.html").write_text("<!doctype html><title>Local fixture</title>")
        navigate(target)
        click("Import cookies")
        find("Open an HTTP or HTTPS site before importing cookies")
        dismiss_result("Import cookies")
    navigate(url)
    hidden = h.root / "hidden-source"
    browser.parents[1].rename(hidden)
    click("Import cookies")
    dismiss_result("No browser profiles found")
    hidden.rename(browser.parents[1])
    h.pass_check("unsupported URLs and missing profiles return to an enabled toolbar")
    click("Import cookies")
    find("Import cookies?")
    from PIL import ImageGrab
    ImageGrab.grab(xdisplay=h.env["DISPLAY"]).save(h.root / "import-confirm.png")
    click("Cancel")
    assert_idle()
    assert cli("browser", "title", pane) == "Sign in"
    h.pass_check("cancel leaves the destination signed out")
    for key in ("Escape", "Return"):
        click("Import cookies")
        find("Import cookies?")
        press(key)
        assert_idle()
        assert_signed_out()
    h.pass_check("Escape and default Enter cancel confirmation")

    click("Import cookies")
    find("Import cookies?")
    navigate(url.replace("127.0.0.1", "localhost"))
    click("Import")
    dismiss_result("Page changed")
    navigate(url)
    assert_signed_out()
    h.pass_check("site change during confirmation prevents import")

    click("Import cookies")
    find("Import cookies?")
    (browser / "Cookies").rename(browser / "Cookies.hidden")
    click("Import")
    dismiss_result("Cookie import failed")
    (browser / "Cookies.hidden").rename(browser / "Cookies")
    assert_signed_out()
    h.pass_check("a profile removed after discovery fails cleanly")
    # Fail before keyring I/O, while the progress dialog is still opening.
    writer.execute("UPDATE meta SET value='invalid' WHERE key='version'")
    writer.commit()
    click("Import cookies")
    click("Import")
    find("Cookie import failed")
    dismiss_result("Cookie import failed")
    writer.execute("UPDATE meta SET value='24' WHERE key='version'")
    writer.commit()
    for description, sql, parameters, title in [
        ("empty site", "DELETE FROM cookies WHERE host_key='127.0.0.1'", (), "No matching cookies"),
        ("expired cookies", "UPDATE cookies SET expires_utc=1", (), "No matching cookies"),
        ("unsupported encryption", "UPDATE cookies SET encrypted_value=? WHERE name='sid'", (b"v20fixture",), "Cookie import failed"),
        ("invalid cookie data", "UPDATE cookies SET encrypted_value=X'', value='bad;value' WHERE name='sid'", (), "Cookie import failed"),
    ]:
        writer.execute(sql, parameters)
        writer.commit()
        import_result(title)
        assert_signed_out()
        restore_rows()
        h.pass_check(f"{description} leaves cookies unchanged and allows retry")
    click("Import cookies")
    click("Import")
    find("Cookie import failed")
    dismiss_result("Cookie import failed")
    assert cli("browser", "title", pane) == "Sign in"
    h.pass_check("missing keyring key reports an error without changing login state")
    install_fixture_key()
    writer.execute("UPDATE cookies SET encrypted_value=? WHERE name='sid'", (b"v11bad",))
    writer.commit()
    import_result("Cookie import failed")
    assert_signed_out()
    restore_rows()
    h.pass_check("decryption failure does not partially import cookies")
    keyring.send_signal(signal.SIGSTOP)
    try:
        click("Import cookies")
        click("Import")
        find("Importing cookies…")
        click("Cancel")
        gui.wait_for(lambda: not lookup("Importing cookies…", showing=False),
                     "progress cancelled", timeout=5)
        find("Import cookies", actionable=True)
    finally:
        keyring.send_signal(signal.SIGCONT)
    assert cli("browser", "title", pane) == "Sign in"
    h.pass_check("cancelling pending keyring access dismisses progress without importing")

    keyring.send_signal(signal.SIGSTOP)
    try:
        click("Import cookies")
        click("Import")
        find("Importing cookies…")
        assert not find("Import cookies").get_state_set().contains(Atspi.StateType.SENSITIVE)
        press("Escape")
        assert_idle()
    finally:
        keyring.send_signal(signal.SIGCONT)
    assert_signed_out()
    h.pass_check("Escape cancels pending keyring access and restores the toolbar")

    keyring.send_signal(signal.SIGSTOP)
    try:
        click("Import cookies")
        click("Import")
        find("Importing cookies…")
        navigate(url.replace("127.0.0.1", "localhost"))
    finally:
        keyring.send_signal(signal.SIGCONT)
    dismiss_result("Page changed")
    navigate(url)
    assert_signed_out()
    h.pass_check("site change while reading prevents import")

    def active_surface():
        return next(tab["id"] for workspace in h.tree(sock)
                    for item in workspace["panes"] if item["id"] == pane
                    for tab in item["tabs"] if tab["active"])

    for action in ("switch", "close"):
        source_surface = active_surface()
        source_button = lookup("Import cookies", actionable=True, role=Atspi.Role.PUSH_BUTTON, last=True)
        pane = open_page()
        destination_surface = active_surface()
        h.rpc(sock, "surface_focus", pane=pane, surface=source_surface)
        keyring.send_signal(signal.SIGSTOP)
        try:
            assert source_button.get_action_iface().do_action(0)
            click("Import")
            find("Importing cookies…")
            h.rpc(sock, "surface_focus", pane=pane, surface=destination_surface)
            if action == "close":
                h.rpc(sock, "surface_close", pane=pane, surface=source_surface)
        finally:
            keyring.send_signal(signal.SIGCONT)
        dismiss_result("Page changed")
        assert_signed_out()
        h.pass_check(f"source tab {action} during keyring access prevents import")

    second = browser.parents[1] / "Profile 1/Network/Cookies"
    second.parent.mkdir(parents=True)
    with sqlite3.connect(second) as alternate:
        writer.backup(alternate)
        alternate.execute("DELETE FROM cookies")
    click("Import cookies")
    find("Import cookies?")
    dropdown = lookup("chrome — Default", role=Atspi.Role.TOGGLE_BUTTON)
    assert dropdown and dropdown.get_action_iface().do_action(0)
    find("chrome — Profile 1")
    press("End")
    press("Return")
    gui.wait_for(lambda: lookup("chrome — Profile 1", role=Atspi.Role.COMBO_BOX), "selected source profile")
    click("Import")
    dismiss_result("No matching cookies")
    second.unlink()
    assert_signed_out()
    h.pass_check("source selection reads only the selected profile")

    def prompt_widgets():
        desktop = Atspi.get_desktop(0)
        desktop.clear_cache()
        for app in (desktop.get_child_at_index(index) for index in range(desktop.get_child_count())):
            if "gcr" in app.get_name():
                nodes = list(walk(app))
                if any(node.get_role() == Atspi.Role.PASSWORD_TEXT
                       and node.get_state_set().contains(Atspi.StateType.SHOWING) for node in nodes):
                    return nodes
        return None

    def respond_to_keyring(approve):
        nodes = gui.wait_for(prompt_widgets, "private keyring approval prompt", timeout=15)
        nodes = [node for node in nodes
                 if node.get_state_set().contains(Atspi.StateType.SHOWING)
                 and node.get_state_set().contains(Atspi.StateType.SENSITIVE)]
        if approve:
            entry = next(node for node in nodes if node.get_role() == Atspi.Role.PASSWORD_TEXT)
            assert entry.get_editable_text_iface().set_text_contents("fixture-keyring-password")
        name = "Unlock" if approve else "Cancel"
        button = next(node for node in nodes if node.get_name() == name and node.get_role() == Atspi.Role.PUSH_BUTTON)
        assert button.get_action_iface().do_action(0)
        gui.wait_for(lambda: not prompt_widgets(), "OS keyring response accepted", timeout=10)

    collection = call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service",
                      "ReadAlias", "(s)", ("default",))[0]
    call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service", "Lock", "(ao)", ([collection],))
    click("Import cookies")
    click("Import")
    gui.wait_for(prompt_widgets, "private keyring approval prompt", timeout=15)
    click("Cancel")
    assert_idle()
    respond_to_keyring(True)
    assert_signed_out()
    h.pass_check("late OS approval after Flowmux cancellation does not import cookies")
    call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service", "Lock", "(ao)", ([collection],))
    click("Import cookies")
    click("Import")
    respond_to_keyring(False)
    dismiss_result("Cookie import failed")
    assert_signed_out()
    h.pass_check("denying the OS keyring prompt leaves the browser signed out and retryable")
    click("Import cookies")
    click("Import")
    gui.wait_for(prompt_widgets, "private keyring approval prompt", timeout=15)
    print("CHECK: waiting for the 120-second keyring approval deadline", flush=True)
    gui.wait_for(lambda: lookup("Cookie import failed"), "keyring approval timeout", timeout=135)
    assert keyring.poll() is None, "keyring exited during approval timeout"
    respond_to_keyring(True)
    assert keyring.poll() is None, "keyring exited after a late approval"
    dismiss_result("Cookie import failed")
    assert_signed_out()
    h.pass_check("keyring approval timeout clears progress and a late approval does not import")
    call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service", "Lock", "(ao)", ([collection],))
    click("Import cookies")
    click("Import")
    respond_to_keyring(True)
    find("Cookies imported")
    ImageGrab.grab(xdisplay=h.env["DISPLAY"]).save(h.root / "import-result.png")
    dismiss_result("Cookies imported")
    gui.wait_for(lambda: cli("browser", "title", pane) == "Signed in", "cookie-backed sign-in")
    assert cli("browser", "eval", pane, "document.cookie") in ("", '""')
    h.pass_check("v11 keyring decryption and HttpOnly cookie insertion sign in on reload")
    import_result("Cookies imported")
    assert cli("browser", "title", pane) == "Signed in"
    h.pass_check("repeat import replaces matching cookies and leaves the UI usable")
    assert list(writer.execute("SELECT * FROM cookies")) == original
    h.pass_check("source database remains unchanged while open in WAL mode")
    other_pane = open_page()
    gui.wait_for(lambda: cli("browser", "title", other_pane) == "Signed in", "shared profile login")
    h.pass_check("new browser tabs share the imported login")
    h.close_window(process)
    process, sock = h.window("import-restarted")
    pane = open_page()
    gui.wait_for(lambda: cli("browser", "title", pane) == "Signed in", "persisted login after restart")
    h.pass_check("imported persistent cookie survives isolated Flowmux restart")
    h.close_window(process)

    options["persist_browser_session"] = False
    (config / "options.json").write_text(json.dumps(options))
    process, sock = h.window("import-ephemeral")
    pane = open_page()
    assert_signed_out()
    click("Import cookies")
    find("Import cookies?")
    warning = lookup("Site: 127.0.0.1\nFlowmux profile: Default\n\nMatching cookies will be replaced across tabs in this profile.\nCookies will be lost when this temporary session ends.\nYour system may ask for keyring access.")
    assert warning, "temporary-session warning is missing"
    click("Import")
    dismiss_result("Cookies imported", escape=True)
    gui.wait_for(lambda: cli("browser", "title", pane) == "Signed in", "ephemeral sign-in")
    h.close_window(process)
    process, sock = h.window("import-ephemeral-restarted")
    pane = open_page()
    assert_signed_out()
    h.pass_check("temporary profile warns, imports, and discards cookies after restart")

    for phase in ("confirmation", "reading"):
        assert keyring.poll() is None, "private keyring must be alive before pausing it"
        keyring.send_signal(signal.SIGSTOP)
        try:
            click("Import cookies")
            find("Import cookies?")
            if phase == "reading":
                click("Import")
                find("Importing cookies…")
            close_request()
            assert_idle()
        finally:
            keyring.send_signal(signal.SIGCONT)
        assert_signed_out()
        h.pass_check(f"window close dismisses {phase} without importing or exiting")
    h.close_window(process)
    print("BROWSER_IMPORT_GUI_OK", flush=True)
except Exception:
    keyring_log = h.root / "keyring.log"
    if keyring_log.exists():
        print(keyring_log.read_text(), file=sys.stderr)
    if "Atspi" in globals():
        from PIL import ImageGrab
        ImageGrab.grab(xdisplay=h.env["DISPLAY"]).save(h.root / "failure.png")
        nodes = [{"role": node.get_role_name(), "name": node.get_name()}
                 for node in walk(Atspi.get_desktop(0)) if node.get_name()]
        (h.root / "failure-accessibility.json").write_text(json.dumps(nodes, indent=2))
    raise
finally:
    if server:
        server.shutdown()
    if writer:
        writer.close()
    h.cleanup()
