#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Linux idle/churn probe using an isolated Flowmux, Xvfb and D-Bus.

Requires python3-xlib, Pillow, python3-gi and gir1.2-atspi-2.0.
Example: --gui target/fast/flowmux --cli target/fast/flowmuxctl
         --cycles 30 --idle-seconds 1800 --renderer gl
RSS is a regression signal, not proof of a heap leak. JSONL samples include
per-process RSS/PSS, FDs and threads; WebKit processes must disappear on close.
"""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import statistics
import sys
import time

from Xlib import X, display
from PIL import Image

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "gui", Path(__file__).with_name("test-ssh-workspace-gui.py")
)
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


def process_sample(pid):
    root = Path(f"/proc/{pid}")
    status = dict(line.split(":", 1) for line in (root / "status").read_text().splitlines())
    memory = dict(line.split(":", 1) for line in (root / "smaps_rollup").read_text().splitlines()[1:])
    return {"pid": pid, "name": status["Name"].strip(),
            "rss_kib": int(memory["Rss"].split()[0]),
            "pss_kib": int(memory["Pss"].split()[0]),
            "private_kib": sum(int(memory[k].split()[0]) for k in ("Private_Clean", "Private_Dirty")),
            "fds": len(list((root / "fd").iterdir())), "threads": int(status["Threads"])}


def descendants(pid):
    found, pending = set(), [pid]
    while pending:
        parent = pending.pop()
        for task in Path(f"/proc/{parent}/task").glob("*"):
            try:
                children = {int(child) for child in (task / "children").read_text().split()}
            except FileNotFoundError:
                continue
            pending.extend(children - found)
            found.update(children)
    return found


def check_growth(samples, rss_mib):
    """Compare settled groups, allowing allocator/cache warm-up before sampling."""
    assert len(samples) >= 6, "need at least six settled samples"
    width = max(2, len(samples) // 3)
    changes = {key: statistics.median(s[key] for s in samples[-width:])
               - statistics.median(s[key] for s in samples[:width])
               for key in ("rss_kib", "fds", "threads", "child_pss_kib", "child_count")}
    assert changes["rss_kib"] <= rss_mib * 1024, f"RSS growth exceeds {rss_mib} MiB: {changes}"
    assert changes["child_pss_kib"] <= rss_mib * 1024, f"child PSS growth exceeds {rss_mib} MiB: {changes}"
    assert changes["fds"] <= 4, f"FDs accumulate: {changes}"
    assert changes["threads"] <= 4, f"threads accumulate: {changes}"
    assert changes["child_count"] <= 1, f"child processes accumulate: {changes}"
    return changes


def self_check():
    samples = [{"rss_kib": 100, "fds": 20, "threads": 4, "child_pss_kib": 100, "child_count": 2}] * 6
    assert all(value == 0 for value in check_growth(samples, 1).values())
    for key, growth in (("rss_kib", 2048), ("fds", 10), ("threads", 10),
                        ("child_pss_kib", 2048), ("child_count", 2)):
        leaking = samples[:3] + [dict(samples[0], **{key: samples[0][key] + growth})] * 3
        try:
            check_growth(leaking, 1)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"growth detector missed {key}")
    assert process_sample(os.getpid())["rss_kib"] > 0
    assert os.getpid() not in descendants(os.getpid())
    print("IDLE_STABILITY_SELF_CHECK_OK", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    parser.add_argument("--cycles", type=int, default=6)
    parser.add_argument("--idle-seconds", type=float, default=30)
    parser.add_argument("--renderer", choices=("cairo", "gl"), default="cairo")
    parser.add_argument("--max-growth-mib", type=float, default=64)
    parser.add_argument("--self-check", action="store_true")
    args = parser.parse_args()
    if args.self_check:
        self_check()
        return
    if args.cycles < 6 or args.idle_seconds < 6 or args.max_growth_mib <= 0:
        parser.error("use at least 6 cycles, 6 idle seconds and a positive growth budget")
    for name in ("gui", "cli"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = gui.Harness(args)
    connection = None
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        h.env.update(GTK_A11Y="atspi", GSK_RENDERER=args.renderer, G_DEBUG="fatal-criticals")
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({"default_shell": str(shell)}))
        html = h.root / "idle.html"
        html.write_text("""<!doctype html><title>Idle stability fixture</title>
<h1>Idle stability fixture</h1><canvas width=200 height=100></canvas>
<script>let n=0; const c=document.querySelector('canvas').getContext('2d');
setInterval(()=>{c.fillStyle=(++n%2)?'#658':'#853';c.fillRect(0,0,200,100)},100)</script>""")
        h.start_display()
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
        connection = display.Display(h.env["DISPLAY"])
        process, socket = h.window("idle-stability")
        atom = connection.intern_atom("_NET_WM_PID")

        def find_window():
            for candidate in connection.screen().root.query_tree().children:
                pid = candidate.get_full_property(atom, X.AnyPropertyType)
                if (pid is not None and int(pid.value[0]) == process.pid
                        and candidate.get_attributes().map_state == X.IsViewable
                        and candidate.get_geometry().width > 100):
                    return candidate

        native_window = gui.wait_for(find_window, "mapped application window")

        def find(name):
            desktop = Atspi.get_desktop(0)
            pending = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
            pending = [app for app in pending if app and app.get_process_id() == process.pid]
            while pending:
                node = pending.pop()
                if node is not None:
                    if node.get_name() == name:
                        return node
                    pending.extend(node.get_child_at_index(i) for i in range(node.get_child_count()))

        def click(name):
            node = gui.wait_for(lambda: find(name), name)
            assert node.get_action_iface().do_action(0), name

        def office(opened):
            click("AgentOffice" if opened else "Back to workspace")
            gui.wait_for(lambda: bool(find("Back to workspace")) == opened, "office visibility")

        def capture(name):
            geometry = native_window.get_geometry()
            raw = native_window.get_image(0, 0, geometry.width, geometry.height, X.ZPixmap, 0xffffffff)
            pixels = Image.frombytes("RGB", (geometry.width, geometry.height), raw.data, "raw", "BGRX")
            assert len(pixels.getcolors(geometry.width * geometry.height)) > 32, "window did not render"
            pixels.save(h.root / f"{name}.png")

        samples_path = h.root / "memory.jsonl"
        started = time.monotonic()

        def sample(phase):
            assert process.poll() is None, f"Flowmux exited: {process.returncode}; {h.root}"
            start = time.monotonic()
            h.rpc(socket, "ping")
            latency = time.monotonic() - start
            assert latency < 5, f"GUI stalled for {latency:.2f}s"
            main_sample = process_sample(process.pid)
            children = []
            for pid in sorted(descendants(process.pid)):
                try:
                    children.append(process_sample(pid))
                except (FileNotFoundError, ProcessLookupError):
                    pass
            row = {"elapsed": time.monotonic() - started, "phase": phase,
                   "ping_seconds": latency, "main": main_sample, "children": children}
            row["resources"] = dict(main_sample, child_pss_kib=sum(c["pss_kib"] for c in children),
                                    child_count=len(children))
            with samples_path.open("a") as output:
                output.write(json.dumps(row) + "\n")
            h.assert_protected()
            return row

        workspace = h.rpc(socket, "workspace_create", name="Idle office", root=str(h.root))["workspace_created"]["id"]
        h.rpc(socket, "workspace_focus", workspace=workspace)
        pane_data = h.workspace(socket, workspace)["panes"][0]
        pane, surface = pane_data["id"], pane_data["tabs"][0]["id"]
        agent = h.root / "codex"
        shutil.copyfile(shutil.which("sleep"), agent)
        agent.chmod(0o755)
        h.send(socket, pane, f"exec {shlex.quote(str(agent))} infinity")
        h.rpc(socket, "agent_activity_update", pane=pane, surface=surface, agent="codex",
              status="working", source="flowmux:hook", session_id="idle-stability-fixture")
        # Resting residents exercise the 2–4 minute shared-event timer during long runs.
        for index in range(2):
            resident = h.rpc(socket, "surface_create", workspace=workspace, cwd=str(h.root))["surface_created"]
            h.send(socket, resident["pane"], f"exec {shlex.quote(str(agent))} infinity")
            h.rpc(socket, "agent_activity_update", pane=resident["pane"], surface=resident["id"],
                  agent="codex", status="idle", source="flowmux:hook",
                  session_id=f"resting-fixture-{index}")

        def browser_open():
            browser = h.rpc(socket, "browser_open", target_pane=pane, direction="vertical",
                            url=html.as_uri())["browser_pane_opened"]["pane"]
            gui.wait_for(lambda: h.rpc(socket, "browser_title", pane=browser)["browser_result"]["value"]
                         == "Idle stability fixture", "local browser loaded")
            assert "Idle stability fixture" in h.rpc(socket, "browser_snapshot", pane=browser)["browser_result"]["value"]
            return browser

        def no_webkit():
            return not any("WebKit" in child["name"] or child["name"] == "bwrap"
                           for child in sample("settle")["children"])

        settled = []
        for cycle in range(args.cycles + 3):
            # The first three cycles warm GTK, fonts, VTE, WebKit and sprite caches.
            tab = h.rpc(socket, "surface_create", workspace=workspace, cwd=str(h.root))["surface_created"]
            marker = f"OUTPUT_DONE_{cycle}"
            output = f'print(("x" * 120 + "\\n") * 10000); print("OUTPUT_DONE_" + str({cycle}))'
            h.send(socket, tab["pane"],
                   f"python3 -c {shlex.quote(output)}")
            gui.wait_for(lambda: marker in h.screen(socket, tab["pane"]), "terminal output drained")
            h.rpc(socket, "surface_close", pane=tab["pane"], surface=tab["id"])
            browser = browser_open()
            office(True)
            for width, height in ((1100, 700), (1280, 800)):
                native_window.configure(x=0, y=0, width=width, height=height)
                connection.sync()
                gui.wait_for(lambda: native_window.get_geometry().width == width, "native window resized")
                time.sleep(.2)
            sample("office-resize")
            office(False)
            h.rpc(socket, "pane_close", pane=browser)
            gui.wait_for(no_webkit, "WebKit children released", timeout=30)
            time.sleep(.25)
            row = sample("churn")
            if cycle >= 3:
                settled.append(row["resources"])
            print(f"CYCLE {cycle + 1}: RSS={row['main']['rss_kib']} KiB FDs={row['main']['fds']}", flush=True)
        changes = {"churn": check_growth(settled, args.max_growth_mib)}
        h.pass_check("terminal/browser/AgentOffice churn releases WebKit children and bounds settled resources")

        browser = browser_open()
        office(True)
        time.sleep(3)
        capture("office-before-idle")
        for phase in ("office-idle", "hidden-idle"):
            if phase == "hidden-idle":
                native_window.unmap()
                connection.sync()
                gui.wait_for(lambda: native_window.get_attributes().map_state != X.IsViewable, "native window hidden")
            idle = []
            deadline = time.monotonic() + args.idle_seconds
            while time.monotonic() < deadline:
                idle.append(sample(phase)["resources"])
                time.sleep(min(5, args.idle_seconds / 6))
            changes[phase] = check_growth(idle, args.max_growth_mib)
            h.pass_check(f"{phase}: {args.idle_seconds}s responsive with bounded resources")
        native_window.map()
        connection.sync()
        gui.wait_for(lambda: native_window.get_attributes().map_state == X.IsViewable, "native window restored")
        time.sleep(.5)
        capture("office-after-idle")
        office(False)
        assert "Idle stability fixture" in h.rpc(socket, "browser_snapshot", pane=browser)["browser_result"]["value"]
        # Kill only this test GUI's renderer, then exercise the user's Reload path.
        renderers = [child["pid"] for child in sample("before-renderer-exit")["children"]
                     if child["name"].startswith("WebKitWeb")]
        assert renderers, "browser renderer was never started"
        for pid in renderers:
            descriptor = os.pidfd_open(pid)
            try:
                assert pid in descendants(process.pid), "renderer is no longer owned by the test"
                signal.pidfd_send_signal(descriptor, signal.SIGKILL)
            finally:
                os.close(descriptor)
        gui.wait_for(lambda: not set(renderers).intersection(descendants(process.pid)), "renderer exited")
        sample("renderer-exited")
        h.rpc(socket, "browser_reload", pane=browser)
        ready = h.rpc(socket, "browser_wait", pane=browser,
                      condition={"Text": "Idle stability fixture"}, timeout_ms=10000, poll_ms=100)
        assert ready["browser_bool_result"]["value"], ready
        h.pass_check("browser renderer exit preserves the GUI; Reload restores page content")
        h.rpc(socket, "pane_close", pane=browser)
        gui.wait_for(no_webkit, "final WebKit teardown")
        h.close_window(process)
        assert not list((h.root / "state/flowmux/crash").glob("crash-*.txt")), "crash report written"
        (h.root / "summary.json").write_text(json.dumps({"renderer": args.renderer,
            "cycles": args.cycles, "idle_seconds_per_phase": args.idle_seconds,
            "growth": changes, "samples": str(samples_path)}, indent=2) + "\n")
        print("IDLE_STABILITY_OK", flush=True)
    finally:
        if connection:
            connection.close()
        h.cleanup()


if __name__ == "__main__":
    main()
