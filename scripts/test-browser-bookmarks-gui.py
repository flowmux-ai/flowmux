#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify bookmark menu refresh and write errors in an isolated Linux GUI.

Requires Xvfb, D-Bus, python3-xlib, GI/Atspi and a non-root user (chmod must
deny writes). Reuses the SSH GUI Harness; no SSH fixture or network is needed.
Artifacts remain in the printed temporary directory, including on failure.
"""
import argparse
from contextlib import closing
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import stat
import sys
import time

sys.dont_write_bytecode = True


def exercise(h, gui, Atspi):
    process, sock = h.window("bookmarks")
    from Xlib import X, display
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
    workspace = h.rpc(sock, "workspace_create", name="Bookmarks", root=str(h.root))["workspace_created"]["id"]
    h.rpc(sock, "workspace_focus", workspace=workspace)
    source = h.workspace(sock, workspace)["panes"][0]["id"]
    tabs = []
    for title in ("Bookmark fixture A", "Bookmark fixture B"):
        page = h.root / (title[-1] + ".html")
        page.write_text(f"<!doctype html><title>{title}</title><p>Local bookmark fixture</p>")
        pane = h.rpc(sock, "browser_open", target_pane=source, direction="vertical",
                     url=page.as_uri())["browser_pane_opened"]["pane"]
        gui.wait_for(lambda: h.rpc(sock, "browser_title", pane=pane)["browser_result"]["value"] == title,
                     f"{title} loaded")
        surface = next(tab["id"] for item in h.workspace(sock, workspace)["panes"]
                       if item["id"] == pane for tab in item["tabs"] if tab["active"])
        tabs.append((pane, surface))
    assert tabs[0][0] == tabs[1][0] and tabs[0][1] != tabs[1][1], tabs

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

    def click(name, parent=None):
        node = gui.wait_for(lambda: named(name, parent, actionable=True), name)
        assert node.get_state_set().contains(Atspi.StateType.SENSITIVE), name
        if name == "Bookmarks":
            # Xvfb can report a zero screen origin; translate window coordinates.
            bounds = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            assert bounds.width > 0 and bounds.height > 0, name
            with closing(display.Display(h.env["DISPLAY"])) as connection:
                native = connection.create_resource_object("window", window.id)
                native.set_input_focus(X.RevertToParent, X.CurrentTime)
                point = connection.screen().root.translate_coords(
                    native, bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
                h.log("native-menu-click", x=point.x, y=point.y)
                xtest.fake_input(connection, X.MotionNotify, x=point.x, y=point.y)
                xtest.fake_input(connection, X.ButtonPress, 1)
                connection.sync()
                time.sleep(.05)
                xtest.fake_input(connection, X.ButtonRelease, 1)
                connection.sync()
        else:
            action = node.get_action_iface()
            assert action and action.get_n_actions() and action.do_action(0), name
        h.log("gui-click", name=name, role=node.get_role_name())
        return node

    def open_menu(tab):
        h.rpc(sock, "surface_focus", pane=tab[0], surface=tab[1])
        # The pane GtkStack crossfades tabs; IPC focus precedes the final frame.
        time.sleep(.4)
        click("Bookmarks")
        try:
            add = gui.wait_for(lambda: named("Bookmark this page", actionable=True), "bookmark menu loaded")
        except AssertionError:
            h.log("menu-open-failure", nodes=[{"name": node.get_name(), "description": node.get_description(),
                  "role": node.get_role_name(), "showing": node.get_state_set().contains(Atspi.StateType.SHOWING)}
                  for node in walk(application()) if node.get_name() or node.get_description()])
            raise
        return add.get_parent()

    def close_menu():
        click("Bookmarks")
        gui.wait_for(lambda: named("Bookmark this page") is None, "bookmark menu closed")

    # Both menus must exist and have been opened before the first mutation.
    for tab in tabs:
        menu = open_menu(tab)
        gui.wait_for(lambda: named("No bookmarks yet", menu), "initially empty menu")
        close_menu()
    menu = open_menu(tabs[0])
    click("Bookmark this page", menu)
    gui.wait_for(lambda: named("Bookmark fixture A", menu), "A displays its added bookmark")
    paths = list((h.root / "data/flowmux/browser").glob("*/bookmarks.json"))
    assert len(paths) == 1, paths
    saved = paths[0]
    expected = [{"title": "Bookmark fixture A", "url": (h.root / "A.html").as_uri()}]
    assert json.loads(saved.read_text()) == expected
    close_menu()

    menu = open_menu(tabs[1])
    gui.wait_for(lambda: named("Bookmark fixture A", menu), "B sees bookmark added in already-created A")
    click("Remove bookmark", menu)
    gui.wait_for(lambda: named("No bookmarks yet", menu), "B displays successful removal")
    assert json.loads(saved.read_text()) == []
    close_menu()
    menu = open_menu(tabs[0])
    gui.wait_for(lambda: named("No bookmarks yet", menu), "A sees bookmark removed in already-created B")
    assert named("Bookmark fixture A", menu) is None
    h.pass_check("already-created same-profile tabs refresh additions and removals when reopened")

    click("Bookmark this page", menu)
    gui.wait_for(lambda: named("Bookmark fixture A", menu), "saved bookmark restored for write failure")
    before = saved.read_bytes()
    assert json.loads(before) == expected
    parent_mode = stat.S_IMODE(saved.parent.stat().st_mode)
    try:
        saved.parent.chmod(0o500)
        assert not os.access(saved.parent, os.W_OK), "write-failure fixture requires a non-root user"
        assert saved.read_bytes() == before, "bookmark must remain readable during the failed write"
        click("Remove bookmark", menu)

        def visible_error():
            labels = [node.get_name() for node in walk(menu)
                      if node.get_role() == Atspi.Role.LABEL
                      and node.get_state_set().contains(Atspi.StateType.SHOWING)]
            text = "\n".join(labels)
            lower = text.lower()
            return text if ("bookmark" in lower
                            and any(word in lower for word in ("failed", "could not", "unable"))
                            and any(word in lower for word in ("permission", "writable", "retry"))) else None

        error = gui.wait_for(visible_error, "actionable visible bookmark write error")
        h.log("bookmark-write-error", text=error)
        assert saved.read_bytes() == before, "failed removal changed saved data"
    finally:
        saved.parent.chmod(parent_mode)
    close_menu()
    menu = open_menu(tabs[1])
    gui.wait_for(lambda: named("Bookmark fixture A", menu), "saved bookmark survives failed removal")
    assert saved.read_bytes() == before
    click("Remove bookmark", menu)
    gui.wait_for(lambda: named("No bookmarks yet", menu), "removal works after permissions restored")
    assert json.loads(saved.read_text()) == []
    h.pass_check("failed write shows an actionable error, preserves readable data, and permits retry")
    close_menu()
    h.close_window(process)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    if os.geteuid() == 0:
        parser.error("run as a non-root user so directory permissions can deny bookmark writes")
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
