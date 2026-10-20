#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise remote Code Review against ssh-workspace-fixture.py --keep.

Uses the existing private Xvfb/D-Bus/XDG harness. Requires python-xlib, Pillow
and the Atspi GI binding. Pass --ssh-config and --remote-cwd from the fixture.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import sys
import time
import uuid

from PIL import Image
from Xlib import X, XK, display
from Xlib.ext import xtest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("gui", Path(__file__).with_name("test-ssh-workspace-gui.py"))
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--ssh-config", required=True)
    parser.add_argument("--host", default="flowmux-fixture")
    parser.add_argument("--remote-cwd", required=True)
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    for name in ("gui", "cli", "ssh_config"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    # The test owns only this new directory, even when pointed at an external fixture.
    args.remote_cwd += "/review 한글 ' $(touch injected) " + uuid.uuid4().hex
    h = gui.Harness(args)
    h.env.update(GTK_A11Y="atspi", GTK_USE_PORTAL="0")
    h.env.pop("AT_SPI_BUS_ADDRESS", None)
    d = None
    try:
        setup = '''import os, pathlib, subprocess
root = pathlib.Path(%r)
root.mkdir()
os.chdir(root)
env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null")
def git(*args):
    return subprocess.run(["git", *args], env=env, check=True, capture_output=True, text=True).stdout
git("-c", "init.templateDir=", "init", "-b", "main")
git("config", "user.name", "SSH review fixture")
git("config", "user.email", "test@example.invalid")
(root / "app.py").write_text("before\\n")
(root / "staged.txt").write_text("initial\\n")
git("add", ".")
git("commit", "-m", "review baseline")
(root / "committed.txt").write_text("REMOTE_COMMIT\\n")
git("add", "committed.txt")
git("commit", "-m", "remote history entry")
(root / "app.py").write_text("REMOTE_WORKING\\n")
(root / "staged.txt").write_text("REMOTE_STAGED\\n")
git("add", "staged.txt")
(root / "new ' $(touch injected).txt").write_text("REMOTE_UNTRACKED\\n")
os.symlink(b"app.py\\n\\xff\\n", b"link.txt")
print(git("rev-parse", "HEAD").strip())
print(git("status", "--porcelain=v1"))
'''
        output = h.remote("python3 -c " + shlex.quote(setup % args.remote_cwd)).stdout
        oid = output.splitlines()[0]
        (h.root / "remote-before.txt").write_text(output)
        clean = h.root / "clean-shell"
        clean.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        clean.chmod(0o755)
        (h.root / "config/flowmux").mkdir()
        (h.root / "config/flowmux/options.json").write_text(json.dumps({
            "default_shell": str(clean), "terminal_minimap_enabled": False,
            "system_notifications_enabled": False,
        }))
        h.start_display()
        os.environ["DISPLAY"] = h.env["DISPLAY"]
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        os.environ.pop("AT_SPI_BUS_ADDRESS", None)
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
        proc, sock = h.window("ssh-review")
        d = display.Display(h.env["DISPLAY"])

        def walk(node):
            node.clear_cache()
            yield node
            for i in range(node.get_child_count()):
                child = node.get_child_at_index(i)
                if child:
                    yield from walk(child)

        def named(name, role=None):
            return next((n for n in walk(Atspi.get_desktop(0))
                         if n.get_state_set().contains(Atspi.StateType.SHOWING)
                         and name in (n.get_name(), n.get_description())
                         and (role is None or n.get_role() == role)), None)

        def find(name, role=None):
            return gui.wait_for(lambda: named(name, role), name)

        def shot(name):
            root = d.screen().root
            g = root.get_geometry()
            raw = root.get_image(0, 0, g.width, g.height, X.ZPixmap, 0xffffffff)
            Image.frombytes("RGB", (g.width, g.height), raw.data, "raw", "BGRX").save(h.root / name)

        def click(node):
            assert node.get_state_set().contains(Atspi.StateType.SENSITIVE), node.get_name()
            if node.get_role() == Atspi.Role.LABEL:
                row = node.get_parent()
                if row.get_role() == Atspi.Role.LIST_ITEM and row.get_parent().get_name() == "Changed files":
                    assert Atspi.Selection.select_child(row.get_parent(), row.get_index_in_parent())
                    return
            action = node.get_action_iface()
            if node.get_role() not in (Atspi.Role.LABEL, Atspi.Role.LIST_ITEM) and action and action.get_n_actions():
                assert action.do_action(0), node.get_name()
            else:
                bounds = node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
                if bounds.x == bounds.y == 0:
                    bounds = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                assert bounds.width > 0 and bounds.height > 0
                h.log("click", name=node.get_name(), x=bounds.x + bounds.width // 2, y=bounds.y + bounds.height // 2)
                xtest.fake_input(d, X.MotionNotify, x=bounds.x + bounds.width // 2, y=bounds.y + bounds.height // 2)
                xtest.fake_input(d, X.ButtonPress, 1)
                xtest.fake_input(d, X.ButtonRelease, 1)
                d.sync()
            time.sleep(.15)

        def key(*names):
            main_window.set_input_focus(X.RevertToParent, X.CurrentTime)
            d.sync()
            codes = [d.keysym_to_keycode(XK.string_to_keysym(name)) for name in names]
            assert all(codes)
            for code in codes:
                xtest.fake_input(d, X.KeyPress, code)
            for code in reversed(codes):
                xtest.fake_input(d, X.KeyRelease, code)
            d.sync()
            time.sleep(.15)

        def patch_contains(value):
            def ready():
                node = named("Diff with old and new line numbers")
                return node and value in Atspi.Text.get_text(node, 0, -1)
            gui.wait_for(ready, "diff contains " + value)

        workspace = h.create(sock)
        pane, tab = h.terminals(h.workspace(sock, workspace))[0]
        h.marker(sock, pane, tab["id"], args.remote_cwd)
        main_window = next(w for w in d.screen().root.query_tree().children
                           if (pid := w.get_full_property(d.intern_atom("_NET_WM_PID"), X.AnyPropertyType)) is not None
                           and int(pid.value[0]) == proc.pid and w.get_geometry().width > 100)
        main_window.set_input_focus(X.RevertToParent, X.CurrentTime)
        d.sync()
        click(find("Pane actions"))
        menu_review = gui.wait_for(lambda: next((n for n in walk(Atspi.get_desktop(0))
            if n.get_name() == "Code Review" and not n.get_description()
            and n.get_role() == Atspi.Role.PUSH_BUTTON
            and n.get_state_set().contains(Atspi.StateType.SHOWING)), None), "review menu")
        click(menu_review)
        patch_contains("REMOTE_WORKING")
        find("4 changed files")
        shot("working-tree.png")
        for label, value in [("M  staged.txt", "REMOTE_STAGED"),
                             ("?  new ' $(touch injected).txt", "REMOTE_UNTRACKED"),
                             ("?  link.txt", "New symbolic link: link.txt → app.py\\n�\\n")]:
            click(find(label, Atspi.Role.LABEL))
            patch_contains(value)
        shot("symlink-path.png")
        h.pass_check("SSH diffs preserve quoted paths and escape symlink targets with control/non-UTF-8 bytes")

        untracked = args.remote_cwd + "/new ' $(touch injected).txt"
        h.remote("python3 -c " + shlex.quote("with open(" + repr(untracked) + ", 'wb') as f: f.truncate(8388609)"))
        click(find("?  new ' $(touch injected).txt", Atspi.Role.LABEL))
        patch_contains("exceeds the 8 MiB display limit")
        click(find("?  link.txt", Atspi.Role.LABEL))
        h.remote("python3 -c " + shlex.quote("import os; os.unlink(" + repr(untracked) + "); os.mkfifo(" + repr(untracked) + ")"))
        click(find("?  new ' $(touch injected).txt", Atspi.Role.LABEL))
        patch_contains("not a regular file")
        h.remote("python3 -c " + shlex.quote("import pathlib; p=pathlib.Path(" + repr(untracked) + "); p.unlink(); p.write_text('REMOTE_UNTRACKED\\n')"))
        h.pass_check("Remote oversized files and paths replaced by FIFOs fail promptly without reading them")

        click(find("Unstaged + Staged", Atspi.Role.TOGGLE_BUTTON))
        row = find(oid[:8] + "  remote history entry", Atspi.Role.LABEL).get_parent()
        click(row)
        patch_contains("REMOTE_COMMIT")
        find("remote history entry", Atspi.Role.LABEL)
        shot("commit-review.png")
        h.pass_check("Remote commit history loads and selecting a commit shows its message and diff")

        click(find("File comment", Atspi.Role.PUSH_BUTTON))
        writer = find("Review comment")
        assert Atspi.EditableText.set_text_contents(writer, "REMOTE_REVIEW_COMMENT")
        click(find("Add comment", Atspi.Role.PUSH_BUTTON))
        find("Comment saved")
        click(find("Refresh changes", Atspi.Role.PUSH_BUTTON))
        patch_contains("REMOTE_COMMIT")
        find("REMOTE_REVIEW_COMMENT")
        h.pass_check("Remote commit comments save and revalidate through SSH on refresh")

        key("Control_L", "Alt_L", "e")
        gui.wait_for(lambda: named("Back to previous tab") is None, "review hides")
        key("Control_L", "Alt_L", "e")
        patch_contains("REMOTE_COMMIT")
        h.pass_check("Ctrl+Alt+E toggles the SSH review and retains its selected commit")

        h.ssh(sock, "disconnect", workspace=workspace)
        if named("Back to previous tab"):
            click(find("Refresh changes", Atspi.Role.PUSH_BUTTON))
            gui.wait_for(lambda: next((n for n in walk(Atspi.get_desktop(0))
                if n.get_state_set().contains(Atspi.StateType.SHOWING)
                and n.get_name().startswith("Git:")), None), "disconnected Git error")
        else:
            key("Control_L", "Alt_L", "e")
            find("Connect this SSH workspace before opening Code Review.")
        h.ssh(sock, "connect", workspace=workspace)
        h.connected(sock, workspace)
        h.rpc(sock, "surface_focus", pane=pane, surface=tab["id"])
        gui.wait_for(lambda: named("Back to previous tab") is None, "reconnected terminal focused")
        key("Control_L", "Alt_L", "e")
        patch_contains("REMOTE_COMMIT")
        find("REMOTE_REVIEW_COMMENT")
        shot("reconnected.png")
        h.pass_check("Disconnect reports an error; reconnect restores remote review and saved comments")

        click(find("File comment", Atspi.Role.PUSH_BUTTON))
        assert Atspi.EditableText.set_text_contents(find("Review comment"), "UNSAVED_REMOTE_COMMENT")
        h.ssh(sock, "disconnect", workspace=workspace)
        h.ssh(sock, "connect", workspace=workspace)
        h.connected(sock, workspace)
        writer = find("Review comment")
        assert Atspi.Text.get_text(writer, 0, -1) == "UNSAVED_REMOTE_COMMENT"
        click(find("Cancel", Atspi.Role.PUSH_BUTTON))
        click(find("Refresh changes", Atspi.Role.PUSH_BUTTON))
        patch_contains("REMOTE_COMMIT")
        find("REMOTE_REVIEW_COMMENT")
        click(find("Send feedback", Atspi.Role.TOGGLE_BUTTON))
        click(find("Copy feedback", Atspi.Role.PUSH_BUTTON))
        find("Feedback copied")
        h.pass_check("Reconnect preserves unsaved comment text; copying feedback revalidates the remote commit")
        after = h.remote("cd " + shlex.quote(args.remote_cwd) + " && git rev-parse HEAD && git status --porcelain=v1").stdout
        assert after.rstrip("\n") == output.rstrip("\n"), (output, after)
        assert h.remote("test ! -e " + shlex.quote(args.remote_cwd + "/injected"), check=False).returncode == 0
        h.pass_check("Review leaves the remote commit, index and working changes unchanged")
        h.close_window(proc)
    except BaseException:
        if d:
            shot("failure.png")
            (h.root / "accessibility.txt").write_text("\n".join(
                f"{n.get_role_name()} showing={n.get_state_set().contains(Atspi.StateType.SHOWING)} name={n.get_name()!r} desc={n.get_description()!r}"
                for n in walk(Atspi.get_desktop(0))))
            print((h.root / "accessibility.txt").read_text(), flush=True)
        raise
    finally:
        if d:
            d.close()
        try:
            h.remote("python3 -c " + shlex.quote("import shutil; shutil.rmtree(" + repr(args.remote_cwd) + ", ignore_errors=True)"), check=False)
        finally:
            h.cleanup()


if __name__ == "__main__":
    main()
