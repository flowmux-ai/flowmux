#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Isolated macOS stress harness for flowmux over raw IPC. Never touches the user's session."""
import argparse, json, os, signal, socket, statistics, subprocess, sys, tempfile, threading, time
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = None
SOCK = None
PROC = None
SAMPLES = []
LATENCIES = []
BASE_CHILDREN = None


class Fixture(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        n = int(self.path.strip("/").split("?")[0] or 0) if self.path.strip("/").split("?")[0].isdigit() else 0
        body = ("<!doctype html><title>page {0}</title><h1>page {0}</h1><style>body{{height:3000px}}</style>".format(n)
                + "".join(f"<p id=p{i}><button>b{i}</button><input aria-label=f{i}></p>" for i in range(300))
                + "<canvas id=c width=800 height=600></canvas><script>const g=c.getContext('2d');let t=0;"
                "function f(){t++;g.fillStyle='hsl('+(t%360)+',80%,50%)';g.fillRect(0,0,800,600);requestAnimationFrame(f)};f();"
                "window.blob=new Array(2e5).fill('x').join('')</script>").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def rpc(verb, timeout=30, **fields):
    started = time.monotonic()
    req = {"id": 1, "kind": "request", "verb": verb, **fields}
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(timeout)
        s.connect(str(SOCK))
        s.sendall((json.dumps(req) + "\n").encode())
        line = s.makefile("rb").readline()
    if not line:
        raise RuntimeError(f"GUI closed IPC during {verb}")
    resp = json.loads(line)
    if resp.get("kind") == "error" or "error" in resp:
        raise RuntimeError(f"{verb} failed: {resp}")
    LATENCIES.append(time.monotonic() - started)
    return resp.get("payload", resp)


def tree():
    return rpc("workspace_tree")["tree"]["workspaces"]


def sample(label):
    global BASE_CHILDREN
    pid = PROC.pid
    deadline = time.monotonic() + 10
    while True:
        children = subprocess.run(["pgrep", "-P", str(pid)], capture_output=True, text=True).stdout.split()
        if BASE_CHILDREN is None or len(children) <= BASE_CHILDREN:
            break
        alive()
        assert time.monotonic() < deadline, f"children did not settle after {label}: {children}"
        time.sleep(.05)
    if BASE_CHILDREN is None:
        BASE_CHILDREN = len(children)
    time.sleep(.25)
    ps = subprocess.run(["ps", "-o", "rss=,vsz=", "-p", str(pid)], capture_output=True, text=True).stdout.split()
    threads = subprocess.run(["ps", "-M", "-p", str(pid)], capture_output=True, text=True).stdout.count("\n") - 1
    descriptors = subprocess.run(["lsof", "-a", "-p", str(pid), "-Ff"],
                                 capture_output=True, text=True, check=True).stdout.splitlines()
    fds = sum(line.startswith("f") and line[1:].isdigit() for line in descriptors)
    row = {"t": round(time.time(), 1), "label": label, "rss_mib": round(int(ps[0]) / 1024, 1),
           "vsz_mib": round(int(ps[1]) / 1024), "threads": threads, "fds": fds, "children": len(children)}
    SAMPLES.append(row)
    print(json.dumps(row), flush=True)
    (ROOT / "samples.jsonl").open("a").write(json.dumps(row) + "\n")
    return row


def alive():
    rc = PROC.poll()
    if rc is not None:
        print(f"!!! GUI EXITED rc={rc}", flush=True)
        print((ROOT / "gui.log").read_text()[-4000:])
        sys.exit(2)


def ws_pane(ws_id):
    for ws in tree():
        if ws["id"] == ws_id:
            return ws["panes"]
    raise RuntimeError("workspace gone")


def wait_screen(pane, needle, timeout=60):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        text = rpc("pane_read_screen", pane=pane).get("screen") or rpc("pane_read_screen", pane=pane)
        if needle in json.dumps(text):
            return True
        time.sleep(0.2)
    return False


def scenario_terminal(ws, cycles, lines):
    for i in range(cycles):
        alive()
        s = rpc("surface_create", workspace=ws, cwd=None)
        pane, surface = s["surface_created"]["pane"], s["surface_created"]["id"]
        time.sleep(0.3)
        marker = f"DO{i}NE"
        cmd = (f"yes 'line of output with some text and escape \\033[31mred\\033[0m' | head -n {lines}; "
               f"for k in $(seq 1 200); do printf '\\033]0;title %s\\007' $k; done; printf 'DO%sNE\\n' {i}\n")
        rpc("pane_send_keys", pane=pane, keys=cmd)
        assert wait_screen(pane, marker), "terminal output did not drain"
        rpc("surface_close", pane=pane, surface=surface)
        sample(f"terminal{i}")


def scenario_split(ws, cycles, depth):
    for i in range(cycles):
        alive()
        base = ws_pane(ws)[0]["id"]
        created = []
        pane = base
        for d in range(depth):
            r = rpc("pane_split", pane=pane, direction="horizontal" if d % 2 else "vertical")
            pane = r["pane_split_done"]["new_pane"]
            created.append(pane)
            rpc("pane_resize", pane=pane, ratio=0.3 + 0.1 * (d % 5))
        time.sleep(0.5)
        for p in reversed(created):
            rpc("pane_close", pane=p)
        sample(f"split{i}")


def scenario_browser(ws, cycles, port, navs):
    for i in range(cycles):
        alive()
        pane = ws_pane(ws)[0]["id"]
        r = rpc("browser_open", url=f"http://127.0.0.1:{port}/0", target_pane=pane, direction="horizontal")
        bpane = r["browser_pane_opened"]["pane"]
        for n in range(navs):
            rpc("browser_navigate", pane=bpane, url=f"http://127.0.0.1:{port}/{n + 1}")
            ready = rpc("browser_wait", pane=bpane, condition={"Text": f"page {n + 1}"},
                        timeout_ms=10000, poll_ms=100)
            assert ready["browser_bool_result"]["value"], ready
            assert rpc("browser_title", pane=bpane)["browser_result"]["value"] == f"page {n + 1}"
            assert "b299" in rpc("browser_snapshot", pane=bpane)["browser_result"]["value"]
        rpc("pane_close", pane=bpane)
        sample(f"browser{i}")


def scenario_agents(ws, cycles):
    panes = ws_pane(ws)
    pane = panes[0]["id"]
    surface = panes[0]["tabs"][0]["id"]
    statuses = ["working", "idle", "blocked", "done"]
    seq = 0
    for i in range(cycles):
        alive()
        for k in range(200):
            seq += 1
            rpc("agent_activity_update", pane=pane, surface=surface, agent=["claude", "codex"][k % 2],
                status=statuses[k % 4], source="flowmux:hook", seq=seq, custom_status=f"step {seq} " + "x" * (k % 50),
                session_id=f"sess-{seq % 7}", session_name=f"name-{seq}", pid=os.getpid())
            if k % 20 == 0:
                rpc("agent_session_update", agent="claude", surface=surface, session_id=f"sess-{seq}")
        sample(f"agents{i}")
    rpc("agent_activity_update", pane=pane, surface=surface, agent="claude", status=None, source="flowmux:hook", seq=seq + 1)


def scenario_notify(ws, cycles):
    panes = ws_pane(ws)
    pane = panes[0]["id"]
    surface = panes[0]["tabs"][0]["id"]
    for i in range(cycles):
        alive()
        for k in range(100):
            rpc("notify", pane=pane, surface=surface, title=f"T{k}", body="body " * (k % 30), level=["info", "turn_completed"][k % 2])
        rpc("notifications_list", unread_only=False)
        rpc("notifications_clear")
        sample(f"notify{i}")


def scenario_ipc_flood(ws, cycles, threads, per):
    pane = ws_pane(ws)[0]["id"]
    def worker(n):
        for k in range(per):
            try:
                [lambda: rpc("ping"), lambda: rpc("workspace_tree"), lambda: rpc("pane_read_screen", pane=pane)][k % 3]()
            except Exception as e:
                return str(e)
        return None
    for i in range(cycles):
        alive()
        with ThreadPoolExecutor(threads) as ex:
            errs = [e for e in ex.map(worker, range(threads)) if e]
        sample(f"ipcflood{i} errs={len(errs)}")
        if errs:
            raise AssertionError(f"flood errors: {errs[:3]}")


def check_growth(rows):
    assert len(rows) >= 6, "need six settled samples"
    width = max(2, len(rows) // 3)
    changes = {key: statistics.median(row[key] for row in rows[-width:])
               - statistics.median(row[key] for row in rows[:width])
               for key in ("rss_mib", "threads", "fds", "children")}
    for key, limit in (("rss_mib", 64), ("threads", 4), ("fds", 4), ("children", 1)):
        assert changes[key] <= limit, f"settled {key} growth: {changes}"
    return changes


def self_check():
    row = dict(rss_mib=200, threads=20, fds=30, children=1)
    assert all(value == 0 for value in check_growth([row] * 6).values())
    for key, growth in (("rss_mib", 65), ("threads", 5), ("fds", 5), ("children", 2)):
        try:
            check_growth([row] * 3 + [dict(row, **{key: row[key] + growth})] * 3)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"missed {key} growth")
    print("MACOS_STABILITY_SELF_CHECK_OK")


def main():
    global ROOT, SOCK, PROC
    ap = argparse.ArgumentParser()
    ap.add_argument("--gui", default="target/debug/flowmux")
    ap.add_argument("--cairo", action="store_true")
    ap.add_argument("--cycles", type=int, default=12)
    ap.add_argument("--self-check", action="store_true")
    ap.add_argument("--only", default="")
    ap.add_argument("--leaks", action="store_true")
    a = ap.parse_args()
    if a.only and not set(a.only.split(",")).issubset({"terminal", "split", "browser", "agents", "notify", "ipc"}):
        ap.error("unknown scenario")
    if a.self_check:
        self_check()
        return
    if sys.platform != "darwin" or a.cycles < 9:
        ap.error("requires macOS and at least 9 cycles (3 warm-up + 6 measured)")
    gui = Path(a.gui).resolve(strict=True)
    cli = gui.with_name("flowmuxctl")
    ROOT = Path(tempfile.mkdtemp(prefix="fm-stress-", dir="/tmp"))
    env = {k: v for k, v in os.environ.items() if not k.startswith("FLOWMUX_")}
    for key, child in [("FLOWMUX_RUNTIME_DIR", "run"), ("XDG_RUNTIME_DIR", "run"), ("XDG_CONFIG_HOME", "config"),
                       ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache"), ("TMPDIR", "tmp")]:
        (ROOT / child).mkdir(mode=0o700, exist_ok=True)
        env[key] = str(ROOT / child)
    env["SHELL"] = "/bin/sh"
    env["HOME"] = str(ROOT / "home"); (ROOT / "home").mkdir(exist_ok=True)
    (ROOT / "state/flowmux").mkdir(parents=True, exist_ok=True)
    (ROOT / "state/flowmux/welcome-seen").touch()
    env["FLOWMUX_BUNDLED_CLI_PATH"] = str(cli)
    env["RUST_BACKTRACE"] = "1"
    env["GSK_RENDERER"] = "cairo" if a.cairo else "gl"
    server = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    log = (ROOT / "gui.log").open("w+")
    PROC = subprocess.Popen([str(gui)], cwd=ROOT, env=env, stdout=log, stderr=log)
    SOCK = ROOT / "run" / f"flowmux-{PROC.pid}.sock"
    print("root", ROOT, "pid", PROC.pid, flush=True)
    try:
        end = time.monotonic() + 60
        while not SOCK.exists():
            alive(); assert time.monotonic() < end; time.sleep(0.1)
        time.sleep(2)
        assert "pong" in rpc("ping")
        ws = rpc("workspace_create", name="stress", root=str(ROOT))["workspace_created"]["id"]
        rpc("workspace_focus", workspace=ws)
        time.sleep(1)
        sample("baseline")
        only = set(a.only.split(",")) if a.only else None
        def on(name):
            return only is None or name in only
        if on("terminal"): scenario_terminal(ws, a.cycles, 200000)
        if on("split"): scenario_split(ws, a.cycles, 6)
        if on("browser"): scenario_browser(ws, a.cycles, server.server_port, 6)
        if on("agents"): scenario_agents(ws, a.cycles)
        if on("notify"): scenario_notify(ws, a.cycles)
        if on("ipc"): scenario_ipc_flood(ws, a.cycles, 16, 100)
        time.sleep(3)
        sample("settled")
        alive()
        growth = {name: check_growth([row for row in SAMPLES if row["label"].startswith(name)][3:])
                  for name in ("terminal", "split", "browser", "agents", "notify", "ipcflood")
                  if any(row["label"].startswith(name) for row in SAMPLES)}
        summary = {"renderer": env["GSK_RENDERER"], "growth": growth,
                   "rpc_p95_ms": sorted(LATENCIES)[int(len(LATENCIES) * .95)] * 1000,
                   "rpc_max_ms": max(LATENCIES) * 1000}
        (ROOT / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps(summary), flush=True)
        if a.leaks:
            out = subprocess.run(["leaks", str(PROC.pid)], capture_output=True, text=True, timeout=600)
            (ROOT / "leaks.txt").write_text(out.stdout + out.stderr)
            assert out.returncode in (0, 1) and "leaks for" in out.stdout, out.stdout + out.stderr
            summary = [l for l in out.stdout.splitlines() if "leaks for" in l or "total leaked" in l]
            print("LEAKS:", summary, flush=True)
    finally:
        if SAMPLES:
            print("first/last:", SAMPLES[0], SAMPLES[-1], flush=True)
        if PROC.poll() is None:
            PROC.send_signal(signal.SIGTERM)
        try:
            rc = PROC.wait(20)
        except subprocess.TimeoutExpired:
            PROC.kill()
            PROC.wait()
            raise AssertionError("isolated GUI failed to stop")
        server.shutdown()
        server.server_close()
        print("exit", rc, flush=True)
        txt = (ROOT / "gui.log").read_text()
        bad = [line for line in txt.splitlines()
               if "panick" in line or "CRITICAL" in line or "ERROR" in line]
        assert not bad, "\n".join(bad[:30])
    print("MACOS_STABILITY_OK", flush=True)


if __name__ == "__main__":
    main()
