#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Real portal/native-input checks in a private nested GNOME Wayland desktop.

Requires gnome-shell, xdg-desktop-portal-gnome, Xvfb, xwininfo, xdotool and ffmpeg.
All consent clicks target the nested test desktop, never the user's desktop.
"""
import ctypes
import importlib.util
import json
import os
import re
import signal
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def run():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/flowmux").resolve()
    with tempfile.TemporaryDirectory(prefix="fm-wl-", dir="/tmp", ignore_cleanup_errors=True) as directory:
        root = Path(directory)
        env = {k: v for k, v in os.environ.items() if not k.startswith("FLOWMUX_")}
        for key, child in [("XDG_RUNTIME_DIR", "run"), ("XDG_CONFIG_HOME", "config"),
                           ("XDG_STATE_HOME", "state"), ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache")]:
            (root / child).mkdir(mode=0o700)
            env[key] = str(root / child)
        env.update(XDG_CURRENT_DESKTOP="GNOME", XDG_SESSION_TYPE="wayland",
                   WAYLAND_DISPLAY="wayland-flowmux-test", GTK_A11Y="test", GSK_RENDERER="cairo")
        return subprocess.run(["xvfb-run", "-a", "-s", "-screen 0 1440x1000x24",
                               "dbus-run-session", "--", sys.executable, "-B", __file__, str(binary),
                               "--nested", str(root)], env=env).returncode


def nested(binary, root):
    with (root / "shell.log").open("w+") as log:
        bus_log = (root / "portal-bus.log").open("w+")
        monitor = subprocess.Popen(["dbus-monitor", "--session",
            "interface='org.freedesktop.portal.FileChooser'",
            "interface='org.freedesktop.portal.Request'",
            "interface='org.freedesktop.impl.portal.FileChooser'",
            "interface='org.freedesktop.impl.portal.Request'", "type='error'", "type='method_return'"], stdout=bus_log, stderr=subprocess.DEVNULL)
        shell = subprocess.Popen(["gnome-shell", "--nested", "--wayland", "--no-x11",
                                  "--wayland-display=wayland-flowmux-test"], stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 30
            while not (root / "run/wayland-flowmux-test").exists():
                assert shell.poll() is None, "nested compositor exited"
                assert time.monotonic() < deadline, "nested compositor startup timed out"
                time.sleep(.1)
            os.environ.update(GDK_BACKEND="wayland", WAYLAND_DISPLAY=str(root / "run/wayland-flowmux-test"))
            spec = importlib.util.spec_from_file_location("browser_suite", Path(__file__).with_name("test-browser-control.py"))
            suite = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(suite)
            task_workflow = suite.exercise_task_workflow
            x11 = ctypes.CDLL("libX11.so.6")
            xtest = ctypes.CDLL("libXtst.so.6")
            x11.XOpenDisplay.restype = ctypes.c_void_p
            x11.XFlush.argtypes = [ctypes.c_void_p]
            xtest.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]
            xtest.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
            display = x11.XOpenDisplay(None)
            assert display, "could not open private Xvfb display"

            def authorize_and_test(cli, url, state):
                workspace = cli("workspace", "new", "--name", "Wayland input", "--root", str(state))["workspace_created"]["id"]
                terminal = next(w for w in cli("tree")["tree"]["workspaces"] if w["id"] == workspace)["panes"][0]["id"]
                pane = cli("browser", "open", url, pane_context=terminal)["browser_pane_opened"]["pane"]

                def browser(op, *args, **kwargs):
                    return cli("browser", op, pane, *args, **kwargs)

                def value(op, *args, **kwargs):
                    return json.loads(browser(op, *args, **kwargs)["browser_result"]["value"])

                def evaluate(source):
                    return browser("eval", source)["browser_result"]["value"]

                def wait_reason(expected):
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        report = value("diagnose")
                        if report["native_input_reason"] == expected:
                            return report
                        time.sleep(.05)
                    path = capture()
                    evidence = Path("target/ci/linux")
                    evidence.mkdir(parents=True, exist_ok=True)
                    (evidence / "wayland-live-failure.png").write_bytes(path.read_bytes())
                    raise AssertionError((expected, report))

                def capture():
                    path = root / "portal.png"
                    subprocess.run(["ffmpeg", "-loglevel", "error", "-f", "x11grab", "-video_size", "1440x1000",
                                    "-i", os.environ["DISPLAY"], "-frames:v", "1", "-y", str(path)], check=True)
                    return path

                def wait_picker():
                    deadline = time.monotonic() + 15
                    while time.monotonic() < deadline:
                        path = capture()
                        if all(suite.png_pixel(path, x, 300) == (250, 250, 250) for x in (80, 720)):
                            return
                    evidence = Path("target/ci/linux")
                    evidence.mkdir(parents=True, exist_ok=True)
                    (evidence / "wayland-live-failure.png").write_bytes(path.read_bytes())
                    raise AssertionError("private portal dialog did not appear")

                def pointer(x, y, click=False):
                    subprocess.run(["xdotool", "windowfocus", window[0]], check=True)
                    # XWarpPointer (xdotool mousemove) does not feed nested Mutter's XI2 input.
                    xtest.XTestFakeMotionEvent(display, -1, x, y, 0)
                    x11.XFlush(display)
                    time.sleep(.2)
                    if click:
                        xtest.XTestFakeButtonEvent(display, 1, 1, 0)
                        xtest.XTestFakeButtonEvent(display, 1, 0, 0)
                        x11.XFlush(display)
                        time.sleep(.2)

                assert browser("wait", "--url", url)["browser_bool_result"]["value"]
                assert browser("wait", "--ready-state", "complete")["browser_bool_result"]["value"]
                report = wait_reason("wayland_permission_required")
                assert report["display_backend"] == "GdkWaylandDisplay" and report["native_input"]["supported"], report
                evaluate("document.body.innerHTML='<input id=f aria-label=Field><button id=b>Click</button>';window.events=[];for(const type of ['keydown','keyup','input','pointerdown','pointerup','click','dblclick'])document.addEventListener(type,e=>events.push([type,e.isTrusted,e.key,e.buttons]));f.focus();'ready'")
                # Only the Xvfb-hosted compositor is focused/clicked, never a host window.
                tree = subprocess.check_output(["xwininfo", "-root", "-tree"], text=True)
                window = re.findall(r'(?m)^\s+(0x[0-9a-f]+) "gnome-shell"', tree)
                assert len(window) == 1, tree
                subprocess.run(["xdotool", "windowfocus", window[0]], check=True)
                for allow in (False, True):
                    error = browser("press", "a", error=True)
                    assert "no input was sent" in str(error), error
                    wait_reason("wayland_permission_pending")
                    wait_picker()
                    if allow:
                        pointer(646, 170, True)  # Remote Interaction switch in the private 800x600 desktop.
                        pointer(686, 90, True)   # Share.
                        pointer(650, 300)
                        wait_reason(None)
                    else:
                        pointer(117, 90, True)   # Cancel.
                        wait_reason("wayland_portal_failed")
                        error = browser("press", "a", error=True)
                        assert "cancelled or denied" in str(error), error
                assert evaluate("JSON.stringify(events)") == "[]", "permission request replayed input"
                browser("press", "a")
                assert evaluate("f.value") == "a"
                browser("press", "Shift+1")
                assert evaluate("f.value") == "a!"
                browser("press", "Ctrl+a")
                browser("press", "Backspace")
                assert evaluate("f.value") == ""
                assert evaluate("String(events.length>0 && events.every(e=>e[1]))") == "true"
                for key, message in [("Shift_L", "Release held"), ("Caps_Lock", "Caps Lock")]:
                    subprocess.run(["xdotool", "keydown" if key == "Shift_L" else "key", key], check=True)
                    time.sleep(.1)
                    try:
                        error = browser("press", "a", error=True)
                        assert message in str(error), error
                    finally:
                        subprocess.run(["xdotool", "keyup" if key == "Shift_L" else "key", key], check=True)
                assert evaluate("f.value") == "", "rejected input changed the field"
                snap = value("snapshot")
                button = next(ref for ref, metadata in snap["refs"].items() if metadata["name"] == "Click")
                browser("click", button)
                browser("hover", button)
                assert evaluate("String(b.matches(':hover'))") == "true"
                browser("dblclick", button)
                assert evaluate("String(events.some(e=>e[0]==='dblclick'&&e[1]))") == "true"
                pointer(20, 15)  # Shell panel, outside the Flowmux surface.
                error = browser("click", button, error=True)
                assert "Place the pointer" in str(error), error
                pointer(650, 300)
                pointer(703, 15, True)  # Stop Sharing in this private GNOME session.
                wait_reason("wayland_portal_failed")
                error = browser("press", "a", error=True)
                assert "revoked" in str(error), error
                browser("press", "a", error=True)
                wait_picker()
                pointer(646, 170, True)
                pointer(686, 90, True)
                pointer(650, 300)
                wait_reason(None)
                print("WAYLAND_PORTAL_INPUT_OK", flush=True)
                native_file = state / "native-picker.txt"
                native_file.write_text("native portal selection")
                evaluate("document.body.insertAdjacentHTML('beforeend','<input type=file id=manual>');manual.onchange=async()=>{window.nativeFile=await manual.files[0].text()};'ready'")
                for select in (True, False):
                    before = (root / "portal-bus.log").stat().st_size
                    evaluate("manual.click();'opened'")
                    deadline = time.monotonic() + 10
                    while True:
                        with (root / "portal-bus.log").open() as observed:
                            observed.seek(before)
                            paths = re.findall(r'interface=org.freedesktop.impl.portal.FileChooser; member=OpenFile\n   object path "([^"]+)"', observed.read())
                        if paths:
                            result = subprocess.run(["gdbus", "introspect", "--session", "--dest", "org.freedesktop.impl.portal.desktop.gnome",
                                "--object-path", paths[-1]], capture_output=True, text=True, timeout=5)
                            if "interface org.freedesktop.impl.portal.Request" in result.stdout:
                                break
                        assert time.monotonic() < deadline, "native picker did not become ready"
                        time.sleep(.025)
                    time.sleep(.2)  # Let the compositor deliver focus after mapping the native window.
                    if select:
                        subprocess.run(["xdotool", "key", "ctrl+l"], check=True)
                        subprocess.run(["xdotool", "type", "--delay", "1", str(native_file)], check=True)
                        subprocess.run(["xdotool", "key", "Return"], check=True)
                    else:
                        subprocess.run(["xdotool", "key", "Escape"], check=True)
                    deadline = time.monotonic() + 10
                    while value("file-chooser", "status")["file_chooser"] is not None:
                        assert time.monotonic() < deadline, "native picker did not complete"
                        time.sleep(.025)
                    assert browser("wait", "--js", "window.nativeFile === 'native portal selection'")["browser_bool_result"]["value"]
                    evaluate("f.value='';f.focus();'ready'")
                    browser("press", "a")
                    assert evaluate("f.value") == "a", "native picker did not return keyboard focus"
                print("WAYLAND_NATIVE_PICKER_RESPONSE_OK", flush=True)
                task_workflow(cli, url, state)

                def assert_pickers_closed():
                    # Verify the actual portal requests, not only Flowmux's pending chooser state.
                    log_text = (root / "portal-bus.log").read_text()
                    paths = set(re.findall(r'interface=org.freedesktop.impl.portal.FileChooser; member=OpenFile\n   object path "([^"]+)"', log_text))
                    assert paths, "no native file chooser requests were observed"
                    for path in paths:
                        deadline = time.monotonic() + 5
                        while True:
                            open_requests = []
                            for service, interface in [("org.freedesktop.portal.Desktop", "org.freedesktop.portal.Request"),
                                                       ("org.freedesktop.impl.portal.desktop.gnome", "org.freedesktop.impl.portal.Request")]:
                                result = subprocess.run(["gdbus", "introspect", "--session", "--dest", service,
                                    "--object-path", path], capture_output=True, text=True, timeout=5)
                                assert result.returncode == 0, result.stderr
                                open_requests.append(f"interface {interface}" in result.stdout)
                            if not any(open_requests):
                                break
                            assert time.monotonic() < deadline, f"file chooser request remains open: {path}"
                            time.sleep(.025)

                assert_pickers_closed()
                print("WAYLAND_PICKER_EARLY_CLOSE_OK", flush=True)

                original = suite.exercise

                def finish(cli, url, state):
                    try:
                        original(cli, url, state)
                    except BaseException:
                        evidence = Path("target/ci/linux")
                        evidence.mkdir(parents=True, exist_ok=True)
                        (evidence / "wayland-live-failure.png").write_bytes(capture().read_bytes())
                        raise
                    assert_pickers_closed()
                    print("WAYLAND_PICKER_CLEANUP_OK", flush=True)
                    cli("workspace", "focus", workspace)
                    # The PID is resolved and checked against this test's private bus.
                    result = subprocess.check_output(["gdbus", "call", "--session", "--dest", "org.freedesktop.DBus",
                        "--object-path", "/org/freedesktop/DBus", "--method", "org.freedesktop.DBus.GetConnectionUnixProcessID",
                        "org.freedesktop.portal.Desktop"], text=True)
                    pid = int(re.search(r"uint32 (\d+)", result).group(1))
                    environment = Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
                    assert ("DBUS_SESSION_BUS_ADDRESS=" + os.environ["DBUS_SESSION_BUS_ADDRESS"]).encode() in environment
                    # Write before stopping: another client may immediately reactivate the portal.
                    config = root / "config/xdg-desktop-portal/portals.conf"
                    config.parent.mkdir(parents=True, exist_ok=True)
                    config.write_text("[preferred]\ndefault=gnome;gtk\norg.freedesktop.impl.portal.RemoteDesktop=none\n")
                    os.kill(pid, signal.SIGTERM)
                    wait_reason("wayland_portal_failed")
                    print("WAYLAND_PORTAL_DISCONNECT_OK", flush=True)

                suite.exercise = finish

            suite.exercise_task_workflow = authorize_and_test
            sys.argv = [str(Path(__file__).with_name("test-browser-control.py")), str(binary)]
            suite.main()

            # A real portal with RemoteDesktop disabled models desktops without that backend.
            config = root / "config/xdg-desktop-portal/portals.conf"
            config.parent.mkdir(parents=True, exist_ok=True)
            config.write_text("[preferred]\ndefault=gnome;gtk\norg.freedesktop.impl.portal.RemoteDesktop=none\n")

            def unsupported(cli, url, state):
                workspace = cli("workspace", "new", "--name", "Unsupported portal", "--root", str(state))["workspace_created"]["id"]
                terminal = next(w for w in cli("tree")["tree"]["workspaces"] if w["id"] == workspace)["panes"][0]["id"]
                pane = cli("browser", "open", url, pane_context=terminal)["browser_pane_opened"]["pane"]
                def browser(op, *args, **kwargs):
                    return cli("browser", op, pane, *args, **kwargs)
                deadline = time.monotonic() + 10
                while True:
                    report = json.loads(browser("diagnose")["browser_result"]["value"])
                    if report["native_input_reason"] == "wayland_portal_unavailable":
                        break
                    assert time.monotonic() < deadline, report
                    time.sleep(.05)
                assert not report["native_input"]["supported"], report
                assert report["native_input_error"] and report["native_text"]["supported"], report
                error = browser("press", "a", error=True)
                assert "RemoteDesktop" in str(error) and "no input was sent" in str(error), error
                assert browser("wait", "--ready-state", "complete")["browser_bool_result"]["value"]
                browser("eval", "document.body.innerHTML='<input id=f>';f.focus();'ready'")
                browser("type", "Native text 한글")
                assert browser("eval", "f.value")["browser_result"]["value"] == "Native text 한글"
                print("WAYLAND_UNSUPPORTED_PORTAL_OK", flush=True)

            suite.exercise_task_workflow = unsupported
            os.environ["FLOWMUX_BROWSER_TASKS_ONLY"] = "1"
            suite.main()
        except BaseException:
            evidence = Path(os.environ.get("CARGO_TARGET_DIR", "target")) / "ci/linux"
            evidence.mkdir(parents=True, exist_ok=True)
            subprocess.run(["ffmpeg", "-loglevel", "error", "-f", "x11grab", "-video_size", "1440x1000",
                            "-i", os.environ["DISPLAY"], "-frames:v", "1", "-y", str(evidence / "wayland-failure.png")])
            log.seek(0)
            (evidence / "wayland-shell.log").write_text(log.read())
            bus_log.flush()
            (evidence / "wayland-portal-bus.log").write_bytes((root / "portal-bus.log").read_bytes())
            raise
        finally:
            monitor.terminate()
            monitor.wait(timeout=5)
            bus_log.close()
            shell.terminate()
            try:
                shell.wait(timeout=10)
            except subprocess.TimeoutExpired:
                shell.kill()
                shell.wait()


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[2] == "--nested":
        nested(Path(sys.argv[1]), Path(sys.argv[3]))
    else:
        sys.exit(run())
