# SPDX-License-Identifier: GPL-3.0-or-later
"""Import regression checks, invoked by the real browser-control GUI suite."""
import os
import json
import sqlite3
import time
from pathlib import Path


def exercise(cli, url, root, profile_name):
    workspace = cli("workspace", "new", "--name", "Browser import", "--root", str(root))["workspace_created"]["id"]
    terminal = next(w for w in cli("tree")["tree"]["workspaces"] if w["id"] == workspace)["panes"][0]["id"]
    pane = cli("browser", "open", url + "import-auth", pane_context=terminal)["browser_pane_opened"]["pane"]
    assert cli("browser", "wait", pane, "--ready-state", "complete")["browser_bool_result"]["value"]
    surface = json.loads(cli("browser", "diagnose", pane)["browser_result"]["value"])["surface_id"]
    source = root / "import-source"
    source.mkdir()
    bookmarks = {"roots": {"bookmark_bar": {"type": "folder", "children": [
        {"type": "url", "name": "Imported 한글", "url": url + "import-auth"},
        {"type": "url", "name": "Duplicate", "url": url + "import-auth"},
        {"type": "url", "name": "Unsafe", "url": "javascript:alert(1)"}]}}}
    (source / "Bookmarks").write_text(json.dumps(bookmarks))
    db = sqlite3.connect(source / "Cookies")
    db.executescript("""CREATE TABLE meta(key TEXT, value TEXT); INSERT INTO meta VALUES('version','24');
        CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,encrypted_value BLOB,path TEXT,
        expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,top_frame_site_key TEXT);""")
    expiry = (int(time.time()) + 3600 + 11644473600) * 1000000
    rows = [
        ("127.0.0.1", "fm_import_session", "authenticated", b"", "/", expiry, 0, 1, 1, ""),
        ("127.0.0.1", "fm_import_pref", "dark", b"", "/", 0, 0, 0, 2, ""),
        ("127.0.0.1", "fm_import_partition", "excluded", b"", "/", expiry, 0, 0, 1, "https://other.test"),
        ("127.0.0.1", "fm_import_expired", "excluded", b"", "/", 11644473601000000, 0, 0, 1, ""),
        ("not127.0.0.1", "fm_import_other", "excluded", b"", "/", expiry, 0, 0, 1, ""),
    ]
    db.executemany("INSERT INTO cookies VALUES(?,?,?,?,?,?,?,?,?,?)", rows)
    db.commit()
    db.close()
    originals = {p.name: p.read_bytes() for p in source.iterdir()}

    def run(*args, error=False):
        response = cli("browser", "--surface", surface, "import", pane, "--from", "chrome", "--source-profile", str(source), *args, error=error)
        return response if error else json.loads(response["browser_result"]["value"])

    report = run("--cookies", "--bookmarks", "--domain", "127.0.0.1")
    assert report == {"status": "complete", "profile": "custom-" + profile_name,
                      "bookmarks_added": 1, "bookmarks_existing": 1, "cookies_added": 2,
                      "cookies_existing": 0, "cookies_skipped_isolated": 1, "cookies_skipped_expired": 1}, report
    assert originals == {p.name: p.read_bytes() for p in source.iterdir()}
    cli("browser", "reload", pane)
    assert cli("browser", "wait", pane, "--text", "IMPORT_AUTHENTICATED")["browser_bool_result"]["value"]
    visible = cli("browser", "eval", pane, "document.cookie")["browser_result"]["value"]
    assert "fm_import_pref=dark" in visible and "fm_import_session" not in visible, visible
    assert "excluded" not in visible, visible
    repeat = run("--cookies", "--bookmarks", "--domain", "127.0.0.1")
    assert repeat["cookies_added"] == repeat["bookmarks_added"] == 0, repeat
    assert repeat["cookies_existing"] == 2, repeat
    # Existing authentication must survive a conflicting import.
    db = sqlite3.connect(source / "Cookies")
    db.execute("UPDATE cookies SET value='must-not-replace' WHERE name='fm_import_session'")
    db.commit()
    db.close()
    assert run("--cookies", "--domain", "127.0.0.1")["cookies_added"] == 0
    cli("browser", "reload", pane)
    assert cli("browser", "wait", pane, "--text", "IMPORT_AUTHENTICATED")["browser_bool_result"]["value"]
    # Parse the entire source before any destination writes.
    (source / "Bookmarks").write_text("malformed")
    run("--bookmarks", "--cookies", error=True)
    run("--cookies", "--domain", "https://invalid", error=True)
    run(error=True)
    # Import into an inactive, explicitly selected tab, preserving the active one.
    cli("browser", "open", url, pane_context=terminal)
    assert run("--cookies", "--domain", "127.0.0.1")["cookies_existing"] == 2
    cli("focus-tab", surface, "--pane", pane)
    # An owned/paused tab must not admit imports from another caller.
    task = json.loads(cli("browser", "control", pane, "start")["browser_result"]["value"])["task_id"]
    run("--cookies", error=True)
    cli("browser", "--task", task, "control", pane, "stop")
    run("--cookies", error=True)
    print("BROWSER_IMPORT_CHROMIUM_HTTP_AND_PRESERVATION_OK", flush=True)

    # Firefox is exercised in a separate tab, so the stopped task stays stopped.
    other = cli("browser", "open", url + "import-auth", pane_context=terminal)["browser_pane_opened"]["pane"]
    firefox = root / "firefox-source"
    firefox.mkdir()
    db = sqlite3.connect(firefox / "cookies.sqlite")
    db.executescript("""CREATE TABLE moz_cookies(host TEXT,name TEXT,value TEXT,path TEXT,expiry INTEGER,
        isSecure INTEGER,isHttpOnly INTEGER,sameSite INTEGER,originAttributes TEXT);
        INSERT INTO moz_cookies VALUES('127.0.0.1','fm_firefox','imported','/',0,0,1,1,''),
        ('127.0.0.1','fm_container','excluded','/',0,0,0,1,'^userContextId=1');""")
    db.commit()
    db.close()
    db = sqlite3.connect(firefox / "places.sqlite")
    db.executescript("""CREATE TABLE moz_places(id INTEGER,title TEXT,url TEXT);
        CREATE TABLE moz_bookmarks(id INTEGER,fk INTEGER,title TEXT,type INTEGER);""")
    db.execute("INSERT INTO moz_places VALUES(1,'Firefox page',?)", (url + "firefox-bookmark",))
    db.execute("INSERT INTO moz_bookmarks VALUES(1,1,'Firefox saved',1)")
    db.commit()
    db.close()
    response = cli("browser", "import", other, "--from", "firefox", "--source-profile", str(firefox), "--cookies", "--bookmarks")
    report = json.loads(response["browser_result"]["value"])
    assert report["cookies_added"] == 1 and report["bookmarks_added"] == 1 and report["cookies_skipped_isolated"] == 1, report
    cli("browser", "reload", other)
    assert cli("browser", "wait", other, "--text", "FIREFOX_AUTHENTICATED")["browser_bool_result"]["value"]
    print("BROWSER_IMPORT_FIREFOX_HTTP_OK", flush=True)

    if actual := os.environ.get("FLOWMUX_IMPORT_CHROME_PROFILE"):
        response = cli("browser", "import", other, "--from", "chrome", "--source-profile", actual, "--cookies", "--domain", "127.0.0.1")
        report = json.loads(response["browser_result"]["value"])
        assert report["cookies_added"] >= 1, report
        cli("browser", "reload", other)
        assert cli("browser", "wait", other, "--text", "REAL_CHROME_AUTHENTICATED")["browser_bool_result"]["value"]
        print("BROWSER_IMPORT_REAL_CHROME_ENCRYPTED_HTTP_OK", flush=True)
