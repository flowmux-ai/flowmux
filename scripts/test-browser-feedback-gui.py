#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise element selection, capture, cancellation and safe agent delivery in an isolated GUI."""
import argparse
from contextlib import closing
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import struct
import sys
import time

sys.dont_write_bytecode = True

def exercise(h, gui, Atspi):
    process, sock = h.window("browser-feedback")
    from Xlib import X, XK, display
    from Xlib.ext import xtest
    with closing(display.Display(h.env["DISPLAY"])) as connection:
        atom = connection.intern_atom("_NET_WM_PID")
        def mapped_window():
            return next((window for window in connection.screen().root.query_tree().children
                         if (pid := window.get_full_property(atom, X.AnyPropertyType)) is not None
                         and int(pid.value[0]) == process.pid
                         and window.get_attributes().map_state == X.IsViewable), None)
        window = gui.wait_for(mapped_window, "isolated GUI window mapped")
        window.configure(width=1600, height=1000)
        connection.sync()
    workspace = h.rpc(sock, "workspace_create", name="Feedback", root=str(h.root))["workspace_created"]["id"]
    h.rpc(sock, "workspace_focus", workspace=workspace)
    source = h.workspace(sock, workspace)["panes"][0]
    terminal, surface = source["id"], source["tabs"][0]["id"]
    page = h.root / "feedback.html"
    page.write_text("""<!doctype html><title>Feedback fixture</title>
<style>body {margin:0} #target {display:block; margin:40px; width:240px; height:100px; background:rgb(20,180,80);border:0}</style>
<button id="target" onclick="document.title='CLICKED'" onkeyup="document.title='KEYUP'">Change this button<span style="display:none">HIDDEN_SECRET</span></button>
<input id="secret" type="password" value="PRIVATE_VALUE">
<input id="edit" value="UNCHANGED" style="position:absolute;top:220px;left:40px">
<iframe style="position:absolute;top:260px;left:40px;width:200px;height:80px" srcdoc="<button onclick=parent.document.title='CLICKED'>Embedded button</button>"></iframe>
""")
    pane = h.rpc(sock, "browser_open", target_pane=terminal, direction="vertical",
                 url=page.as_uri() + "?token=PRIVATE_TOKEN#private")["browser_pane_opened"]["pane"]
    gui.wait_for(lambda: h.rpc(sock, "browser_title", pane=pane)["browser_result"]["value"] == "Feedback fixture", "fixture loaded")
    def walk(node):
        node.clear_cache()
        yield node
        for index in range(node.get_child_count()):
            child = node.get_child_at_index(index)
            if child:
                yield from walk(child)

    def application():
        desktop = Atspi.get_desktop(0)
        desktop.clear_cache()
        return next((desktop.get_child_at_index(index) for index in range(desktop.get_child_count())
                     if desktop.get_child_at_index(index).get_process_id() == process.pid), None)

    def named(name, parent=None, actionable=False):
        root = parent if parent is not None else application()
        matches = [node for node in walk(root)
                     if (node.get_name().split("\n", 1)[0] == name
                         or node.get_description().split("\n", 1)[0] == name)
                     and node.get_state_set().contains(Atspi.StateType.SHOWING)
                     and (not actionable or (node.get_role() in (Atspi.Role.PUSH_BUTTON, Atspi.Role.TOGGLE_BUTTON)
                          and node.get_action_iface() and node.get_action_iface().get_n_actions()))] if root else []
        return matches[-1] if matches else None

    def comment_text():
        # AT-SPI visibility follows GTK mapping asynchronously after reopening.
        node = gui.wait_for(lambda: named("Requested change"), "request editor visible")
        return Atspi.Text.get_text(node, 0, -1)

    def click(name, parent=None):
        node = gui.wait_for(lambda: named(name, parent, actionable=True), name)
        assert node.get_state_set().contains(Atspi.StateType.SENSITIVE), name
        if name == "Send to agent":
            native_click(node)
        else:
            action = node.get_action_iface()
            assert action and action.get_n_actions() and action.do_action(0), name
        h.log("gui-click", name=name, role=node.get_role_name())
        return node


    def native_click(node, x=None, y=None):
        bounds = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        assert bounds.width > 0 and bounds.height > 0
        with closing(display.Display(h.env["DISPLAY"])) as connection:
            native = connection.create_resource_object("window", window.id)
            native.set_input_focus(X.RevertToParent, X.CurrentTime)
            point = connection.screen().root.translate_coords(native, bounds.x + (bounds.width // 2 if x is None else x), bounds.y + (bounds.height // 2 if y is None else y))
            xtest.fake_input(connection, X.MotionNotify, x=point.x, y=point.y)
            xtest.fake_input(connection, X.ButtonPress, 1)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            connection.sync()

    h.rpc(sock, "browser_eval", pane=pane, source="document.querySelector('#edit').focus(); 'ready'")
    click("Request change")
    gui.wait_for(lambda: named("Select an element. Escape cancels; Tab and Enter select a focused element."), "picker started")
    time.sleep(.2)
    click("Request change")
    gui.wait_for(lambda: named("Select an element. Escape cancels; Tab and Enter select a focused element.") is None, "toolbar closes the picker panel")
    gui.wait_for(lambda: h.rpc(sock, "browser_eval", pane=pane, source="String(!!globalThis.__flowmuxFeedback)")["browser_result"]["value"] == "false", "toolbar removes the active picker")
    h.pass_check("pressing Request change again cancels active selection like Close")
    click("Request change")
    time.sleep(.5)
    with closing(display.Display(h.env["DISPLAY"])) as connection:
        key = connection.keysym_to_keycode(XK.string_to_keysym("x"))
        xtest.fake_input(connection, X.KeyPress, key)
        xtest.fake_input(connection, X.KeyRelease, key)
        connection.sync()
    # WebKit's page accessibility tree is optional; use the GTK content bounds
    # and the fixture's known CSS coordinates for an actual native click.
    root = named("Request change").get_parent()
    while root.get_role() != Atspi.Role.SCROLL_PANE:
        root = root.get_parent()
    root = root.get_parent()
    content = root.get_child_at_index(root.get_child_count() - 1)
    native_click(content, 160, 90)
    comment = gui.wait_for(lambda: named("Requested change"), "selection captured")
    assert h.rpc(sock, "browser_eval", pane=pane, source="document.querySelector('#edit').value")["browser_result"]["value"] == "UNCHANGED", "selection mode edited the focused page input"
    assert h.rpc(sock, "browser_title", pane=pane)["browser_result"]["value"] == "Feedback fixture", "selection activated the page button"
    images = list((h.root / "cache/flowmux/browser-feedback").glob("*.png"))
    assert len(images) == 1, images
    image = images[0]
    assert struct.unpack(">II", image.read_bytes()[16:24]) == (240, 100)
    h.pass_check("native element click captures exactly its bounds without activating the page button")
    click("Copy request")
    gui.wait_for(lambda: named("Describe what should change first."), "empty comment rejected")
    assert comment.get_editable_text_iface().set_text_contents("Make this button blue.\nKeep its label.")
    click("Copy request")
    gui.wait_for(lambda: named("Request copied, including the local screenshot path."), "copy confirmed")
    details = "\n".join(node.get_name() for node in walk(application()))
    assert "button#target" in details and "PRIVATE_TOKEN" not in details and "PRIVATE_VALUE" not in details and "HIDDEN_SECRET" not in details
    click("Request change")
    gui.wait_for(lambda: named("Requested change") is None, "toolbar closes the captured request")
    click("Request change")
    comment = gui.wait_for(lambda: named("Requested change"), "toolbar reopens the draft")
    assert "Make this button blue" in Atspi.Text.get_text(comment, 0, -1)
    assert h.rpc(sock, "browser_eval", pane=pane, source="String(!!globalThis.__flowmuxFeedback)")["browser_result"]["value"] == "false"
    h.pass_check("toolbar close preserves the captured request and reopens without picking again")
    click("Select another element")
    time.sleep(.4)
    with closing(display.Display(h.env["DISPLAY"])) as connection:
        key = connection.keysym_to_keycode(XK.string_to_keysym("Escape"))
        xtest.fake_input(connection, X.KeyPress, key)
        xtest.fake_input(connection, X.KeyRelease, key)
        connection.sync()
    gui.wait_for(lambda: named("Selection cancelled. Your previous request is kept."), "Escape cancels")
    assert "Make this button blue" in comment_text()
    # Simulate a page layout update between the native click and async capture.
    h.rpc(sock, "browser_eval", pane=pane, source="window.addEventListener('click', () => setTimeout(() => document.querySelector('#target').style.marginLeft = '300px', 0), {capture:true, once:true}); 'ready'")
    click("Select another element")
    time.sleep(.4)
    native_click(content, 160, 90)
    gui.wait_for(lambda: named("The selected element or viewport changed. Select the element again.")
                 or named("The selected element or viewport changed during capture. Select the element again."), "changed layout rejected")
    assert "Make this button blue" in comment_text()
    assert list((h.root / "cache/flowmux/browser-feedback").glob("*.png")) == [image]
    h.rpc(sock, "browser_eval", pane=pane, source="document.querySelector('#target').style.marginLeft = '40px'; 'reset'")
    h.pass_check("typing during selection cannot edit the page; changed capture geometry preserves the previous draft")
    for y, message in [(190, "Choose a surrounding label or container instead of an input field."),
                       (300, "Embedded content cannot be selected. Choose an element in the main page.")]:
        click("Select another element")
        time.sleep(.4)
        native_click(content, 80, y)
        gui.wait_for(lambda: named(message), "unsafe target rejected")
        assert "Make this button blue" in comment_text()
    assert h.rpc(sock, "browser_title", pane=pane)["browser_result"]["value"] == "Feedback fixture"
    h.pass_check("password fields and embedded content are rejected without losing the existing request")
    click("Close", named("Requested change").get_parent().get_parent().get_parent().get_parent())
    native_click(content, 80, 230)
    with closing(display.Display(h.env["DISPLAY"])) as connection:
        key = connection.keysym_to_keycode(XK.string_to_keysym("x"))
        xtest.fake_input(connection, X.KeyPress, key)
        xtest.fake_input(connection, X.KeyRelease, key)
        connection.sync()
    gui.wait_for(lambda: h.rpc(sock, "browser_eval", pane=pane, source="document.querySelector('#edit').value")["browser_result"]["value"] != "UNCHANGED", "normal editing restored after cancellation")
    click("Request change")
    assert "Make this button blue" in comment_text()
    h.pass_check("empty requests rejected; URL secrets excluded; Escape and panel close preserve the draft")

    receiver = h.root / "codex"
    receiver.write_text("#!" + sys.executable + "\n" + r'''import os, sys, termios, tty
from pathlib import Path
root = Path(sys.argv[1])
old = termios.tcgetattr(0)
tty.setraw(0)
try:
    os.write(1, "\x1b[?2004h\r\n› ".encode())
    (root / 'receiver.pid').write_text(str(os.getpid()))
    data = b''
    while True:
        data += os.read(0, 65536)
        (root / 'receipt.bin').write_bytes(data)
        if data.endswith(b'\x1b[201~\r'):
            break
finally:
    termios.tcsetattr(0, termios.TCSANOW, old)
''')
    receiver.chmod(0o700)
    h.send(sock, terminal, shlex.join([str(receiver), str(h.root)]))
    gui.wait_for(lambda: (h.root / "receiver.pid").exists(), "receiver started")
    pid = int((h.root / "receiver.pid").read_text())
    def report(activity):
        h.rpc(sock, "agent_activity_update", pane=terminal, surface=surface, agent="codex", activity=activity,
              pid=pid, session_id="feedback-fixture", source="flowmux:hook")
    def send_to_fixture():
        click("Send to agent")
        node = gui.wait_for(lambda: next((node for node in walk(application())
            if node.get_name().startswith("codex · Feedback") and node.get_role() == Atspi.Role.PUSH_BUTTON
            and node.get_state_set().contains(Atspi.StateType.SHOWING)), None), "agent target")
        assert node.get_action_iface().do_action(0)
    report("running")
    send_to_fixture()
    gui.wait_for(lambda: named("This agent is busy or waiting for approval. Send again when it is ready."), "busy rejected")
    assert not (h.root / "receipt.bin").exists()
    assert "Make this button blue" in comment_text()
    report("idle")
    send_to_fixture()
    gui.wait_for(lambda: named("Request sent to agent. The captured image is kept for the agent to read."), "delivery confirmed")
    data = gui.wait_for(lambda: (h.root / "receipt.bin").read_bytes(), "PTY receipt")
    assert data.startswith(b"\x1b[200~Requested UI change:") and data.endswith(b"\x1b[201~\r"), data
    assert b"Make this button blue" in data and b"button#target" in data and str(image).encode() in data
    assert b"PRIVATE_TOKEN" not in data and b"PRIVATE_VALUE" not in data
    assert comment_text() == ""
    h.pass_check("busy agent receives no input; retry delivers the comment, context and existing image path exactly once")
    h.rpc(sock, "browser_eval", pane=pane, source="document.querySelector('#target').focus(); 'ready'")
    click("Select another element")
    time.sleep(.4)
    with closing(display.Display(h.env["DISPLAY"])) as connection:
        key = connection.keysym_to_keycode(XK.string_to_keysym("Return"))
        xtest.fake_input(connection, X.KeyPress, key)
        xtest.fake_input(connection, X.KeyRelease, key)
        connection.sync()
    gui.wait_for(lambda: named("Requested change"), "keyboard selection captured")
    assert h.rpc(sock, "browser_title", pane=pane)["browser_result"]["value"] == "Feedback fixture", "selection key activated a page handler"
    h.pass_check("keyboard selection suppresses both page activation and key-release handlers")
    h.close_window(process)
    assert image.exists(), "delivered image must outlive the browser tab"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    for tool in ("Xvfb", "dbus-daemon"):
        if not shutil.which(tool):
            parser.error(f"Required tool unavailable: {tool}")
    for key in ("gui", "cli"):
        setattr(args, key, str(Path(getattr(args, key)).resolve(strict=True)))
    spec = importlib.util.spec_from_file_location("gui", Path(__file__).with_name("test-ssh-workspace-gui.py"))
    gui = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(gui)
    h = gui.Harness(args)
    h.env.update(GTK_A11Y="atspi", GTK_USE_PORTAL="0", LANG="C.UTF-8", LC_ALL="C.UTF-8")
    h.env.pop("AT_SPI_BUS_ADDRESS", None)
    config = h.root / "config/flowmux"
    config.mkdir()
    (config / "options.json").write_text(json.dumps({"default_shell": "/bin/sh"}))

    def interrupted(*_):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupted)
    try:
        h.start_display()
        os.environ["DISPLAY"] = h.env["DISPLAY"]
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        os.environ.pop("AT_SPI_BUS_ADDRESS", None)
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(1500, 1500)
        exercise(h, gui, Atspi)
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
