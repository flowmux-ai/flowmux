#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Test encrypted SSH autofill with a private GUI, keyring and real SSH server.

Use ssh-workspace-fixture.py --keep. Requires GNOME Keyring, GI/Atspi,
python-xlib and Xvfb. Never accesses the desktop's keyring or user windows.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time
import uuid

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "gui", Path(__file__).with_name("test-ssh-workspace-gui.py"))
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--gui", default="target/debug/flowmux")
parser.add_argument("--cli", default="target/debug/flowmuxctl")
parser.add_argument("--ssh-config", required=True)
parser.add_argument("--remote-cwd", required=True)
parser.add_argument("--host", default="flowmux-fixture")
parser.add_argument("--protected-pid", type=int, action="append", default=[])
args = parser.parse_args()
for key in ("gui", "cli", "ssh_config"):
    setattr(args, key, str(Path(getattr(args, key)).resolve(strict=True)))
h = gui.Harness(args)
h.env.update(GTK_A11Y="atspi", GTK_USE_PORTAL="0")
d = None
password = uuid.uuid4().hex

try:
    clean = h.root / "clean-shell"
    clean.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
    clean.chmod(0o755)
    config = h.root / "config/flowmux"
    config.mkdir()
    (config / "options.json").write_text(json.dumps({
        "default_shell": str(clean), "system_notifications_enabled": False,
    }))
    h.env.pop("AT_SPI_BUS_ADDRESS", None)
    h.start_display()
    os.environ["DISPLAY"] = h.env["DISPLAY"]
    os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
    os.environ.pop("AT_SPI_BUS_ADDRESS", None)
    import gi
    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi, Gio, GLib
    from Xlib import X, display, protocol
    from Xlib.ext import xtest
    Atspi.set_timeout(1500, 1500)
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(path, interface, method, signature, values):
        return bus.call_sync("org.freedesktop.secrets", path, interface, method,
                             GLib.Variant(signature, values), None,
                             Gio.DBusCallFlags.NO_AUTO_START, 5000, None).unpack()

    def start_keyring():
        with (h.root / "keyring.log").open("a") as log:
            process = h.spawn([
                "gnome-keyring-daemon", "--foreground", "--unlock",
                "--components=secrets", "--control-directory", str(h.root / "keyring-control"),
            ], stdin=subprocess.PIPE, stdout=log, stderr=log)
        process.stdin.write(password.encode())
        process.stdin.close()
        def ready():
            try:
                return call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service",
                            "ReadAlias", "(s)", ("default",))[0] != "/"
            except GLib.Error:
                return False
        gui.wait_for(ready, "private persistent keyring")
        return process

    keyring = start_keyring()

    def items():
        unlocked, locked = call(
            "/org/freedesktop/secrets", "org.freedesktop.Secret.Service",
            "SearchItems", "(a{ss})", ({"service": "com.flowmux.App.ssh-history.v1"},))
        assert not locked
        return unlocked

    def session():
        return call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service",
                    "OpenSession", "(sv)", ("plain", GLib.Variant("s", "")))[1]

    def secret():
        found = items()
        if not found:
            return None
        assert len(found) == 1
        ss = session()
        try:
            result = call(found[0], "org.freedesktop.Secret.Item", "GetSecret", "(o)", (ss,))[0]
            return json.loads(bytes(result[2]))
        finally:
            call(ss, "org.freedesktop.Secret.Session", "Close", "()", ())

    process, sock = h.window("history-first")
    h.rpc(sock, "workspace_create", name="Local fixture", root=str(h.root), team=False)
    d = display.Display(h.env["DISPLAY"])

    def walk(node):
        node.clear_cache()
        yield node
        for i in range(node.get_child_count()):
            child = node.get_child_at_index(i)
            if child:
                yield from walk(child)

    def nodes():
        desktop = Atspi.get_desktop(0)
        desktop.clear_cache()
        app = next((desktop.get_child_at_index(i) for i in range(desktop.get_child_count())
                    if desktop.get_child_at_index(i).get_process_id() == process.pid), None)
        return [] if app is None else [
            n for n in walk(app) if n.get_state_set().contains(Atspi.StateType.SHOWING)]

    def find(name, role=None):
        return gui.wait_for(lambda: next((n for n in reversed(nodes()) if n.get_name() == name
                                         and (role is None or n.get_role() == role)), None),
                            name, timeout=12)

    def click(node, button=1):
        action = node.get_action_iface()
        if button == 1 and node.get_role() not in (Atspi.Role.LABEL, Atspi.Role.LIST_ITEM) \
                and action and action.get_n_actions():
            assert action.do_action(0)
        else:
            b = node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
            if b.x == b.y == 0:
                b = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            xtest.fake_input(d, X.MotionNotify, x=b.x + b.width // 2, y=b.y + b.height // 2)
            xtest.fake_input(d, X.ButtonPress, button)
            xtest.fake_input(d, X.ButtonRelease, button)
            d.sync()

    def loaded():
        return any(n.get_name().startswith((
            "Last successful connection loaded", "Successful connections are remembered",
            "SSH history unavailable.", "Saved SSH history is invalid;")) for n in nodes())

    def open_new(wait_loaded=True):
        # Right-click the workspace row to use the same New SSH Workspace menu.
        rows = gui.wait_for(lambda: [n for n in nodes() if n.get_role() == Atspi.Role.LIST_ITEM],
                            "workspace row")
        click(rows[0], button=3)
        click(find("New SSH Workspace"))
        find("Host", Atspi.Role.TEXT)
        if wait_loaded:
            gui.wait_for(loaded, "history loaded")

    def value(label):
        node = find(label, Atspi.Role.TEXT)
        return Atspi.Text.get_text(node, 0, Atspi.Text.get_character_count(node))

    def fill(label, text):
        assert find(label, Atspi.Role.TEXT).get_editable_text_iface().set_text_contents(text)

    def close_dialog():
        for window in d.screen().root.query_tree().children:
            if window.get_wm_name() == "New SSH Workspace":
                window.send_event(protocol.event.ClientMessage(
                    window=window, client_type=d.intern_atom("WM_PROTOCOLS"),
                    data=(32, [d.intern_atom("WM_DELETE_WINDOW"), X.CurrentTime, 0, 0, 0])))
        d.sync()
        gui.wait_for(lambda: not any(n.get_name() == "Host" for n in nodes()), "dialog closed")

    open_new()
    assert value("Host") == "" and not items()
    # Read the fixture's resolved port and identity without modifying its config.
    effective = subprocess.check_output(
        ["ssh", "-G", "-F", args.ssh_config, args.host], text=True)
    settings = dict(line.split(" ", 1) for line in effective.splitlines() if " " in line)
    expected = {
        "Host": args.host,
        "Remote directory": args.remote_cwd,
        "Workspace name": "암호화 SSH " + uuid.uuid4().hex,
        "Port": settings["port"],
        "Identity file": settings["identityfile"],
        "SSH config file": args.ssh_config,
    }
    for label, text in expected.items():
        fill(label, text)
    click(find("Keep remote sessions with tmux", Atspi.Role.CHECK_BOX))
    existing = {ws["id"] for ws in h.tree(sock)}
    click(find("Connect", Atspi.Role.PUSH_BUTTON))
    workspace = gui.wait_for(lambda: next((ws["id"] for ws in h.tree(sock)
                                          if ws["id"] not in existing), None), "created SSH workspace")
    h.connected(sock, workspace)
    for _, tab in h.terminals(h.workspace(sock, workspace)):
        h.sessions.add("flowmux-" + tab["id"].replace("-", ""))
    saved = gui.wait_for(secret, "successful settings saved")
    assert saved["name"] == expected["Workspace name"]
    assert saved["config"]["target"]["host"] == args.host and saved["config"]["tmux"]
    h.pass_check("New SSH Workspace saves all form inputs after a real SSH connection succeeds")

    keyring_files = list((h.root / "data/keyrings").glob("*.keyring"))
    assert keyring_files
    for path in keyring_files:
        contents = path.read_bytes()
        assert contents.startswith(b"GnomeKeyring\n\r\0\n"), path
        for text in expected.values():
            if len(text) > 5:
                assert text.encode() not in contents, (path, text)
    attrs = call(items()[0], "org.freedesktop.DBus.Properties", "Get",
                 "(ss)", ("org.freedesktop.Secret.Item", "Attributes"))[0]
    assert expected["Workspace name"] not in str(attrs) and args.ssh_config not in str(attrs)
    h.pass_check("On-disk keyring is encrypted; connection details are absent from metadata")

    # Restart both our GUI and keyring: proves disk persistence, not an in-memory cache.
    h.close_window(process)
    h.stop(keyring)
    keyring = start_keyring()
    process, sock = h.window("history-restarted")
    open_new()
    for label, text in expected.items():
        assert value(label) == text, (label, value(label), text)
    assert find("Keep remote sessions with tmux", Atspi.Role.CHECK_BOX).get_state_set().contains(Atspi.StateType.CHECKED)
    h.pass_check("All fields and tmux are prefilled after GUI and keyring restart")
    close_dialog()

    # A delayed read must not erase typing that started while the keyring was busy.
    os.kill(keyring.pid, signal.SIGSTOP)
    try:
        open_new(wait_loaded=False)
        fill("Host", "typing.example")
    finally:
        os.kill(keyring.pid, signal.SIGCONT)
    gui.wait_for(loaded, "delayed history read")
    assert value("Host") == "typing.example" and value("Remote directory") == ""
    close_dialog()
    assert secret() == saved
    h.pass_check("A delayed keyring read preserves edits already made in the dialog")
    open_new()

    expected.update({
        "Workspace name": "마지막 성공 " + uuid.uuid4().hex,
        "Remote directory": "", "Port": "", "Identity file": "",
    })
    for label, text in expected.items():
        fill(label, text)
    click(find("Keep remote sessions with tmux", Atspi.Role.CHECK_BOX))
    before = {ws["id"] for ws in h.tree(sock)}
    click(find("Connect", Atspi.Role.PUSH_BUTTON))
    latest = gui.wait_for(lambda: next((ws["id"] for ws in h.tree(sock)
                                       if ws["id"] not in before), None), "newest workspace")
    h.connected(sock, latest)
    saved = gui.wait_for(lambda: (data if (data := secret()) and
                                 data["name"] == expected["Workspace name"] else None),
                         "newest successful settings")
    assert saved["config"]["cwd"] is None and saved["config"]["target"]["port"] is None
    assert saved["config"]["target"]["identity_file"] is None and not saved["config"]["tmux"]
    open_new()
    for label, text in expected.items():
        assert value(label) == text, (label, value(label), text)
    assert not find("Keep remote sessions with tmux", Atspi.Role.CHECK_BOX).get_state_set().contains(Atspi.StateType.CHECKED)
    h.pass_check("A later success replaces previous defaults, including cleared optional fields")

    # Hold a port open without listening, so the next connection is refused.
    with socket.socket() as refused:
        refused.bind(("127.0.0.1", 0))
        fill("Host", "127.0.0.1")
        fill("Port", str(refused.getsockname()[1]))
        fill("Workspace name", "failed attempt")
        before = {ws["id"] for ws in h.tree(sock)}
        click(find("Connect", Atspi.Role.PUSH_BUTTON))
        failed = gui.wait_for(lambda: next((ws["id"] for ws in h.tree(sock)
                                           if ws["id"] not in before), None), "failed workspace")
        gui.wait_for(lambda: h.ssh(sock, "status", workspace=failed)["state"] == "failed",
                     "connection refusal")
    assert secret() == saved
    open_new()
    assert value("Host") == expected["Host"] and value("Workspace name") == expected["Workspace name"]
    fill("Host", "cancelled.example")
    close_dialog()
    assert secret() == saved
    h.pass_check("Failed and cancelled attempts preserve the last successful connection")

    # Malformed decrypted data must not become dialog input or delete the entry.
    ss = session()
    call(items()[0], "org.freedesktop.Secret.Item", "SetSecret",
         "((oayays))", ((ss, [], list(b"invalid private settings"), "application/json"),))
    call(ss, "org.freedesktop.Secret.Session", "Close", "()", ())
    open_new()
    assert value("Host") == ""
    find("Saved SSH history is invalid; enter the connection settings again.")
    h.pass_check("Corrupt history leaves an editable empty form and an explicit error")
    close_dialog()
    h.close_window(process)
    h.assert_protected()
except Exception:
    if d:
        from PIL import ImageGrab
        ImageGrab.grab(xdisplay=h.env["DISPLAY"]).save(h.root / "failure.png")
        (h.root / "accessibility.txt").write_text("\n".join(
            f"{n.get_role_name()}: {n.get_name()}" for n in nodes()))
    raise
finally:
    if d:
        d.close()
    h.cleanup()
