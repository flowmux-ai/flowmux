#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Two isolated native GUIs resume one real Codex session and focus its owner.

Copies only the three selected disposable histories and model metadata. No user
credentials are copied and no model prompt is submitted. GUI exits are released
only after the writer and foreign-focus evidence has been checked.
"""
import argparse, json, os, pathlib, shutil, sqlite3, subprocess, sys, tempfile, time
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--seed-home', type=pathlib.Path, required=True)
parser.add_argument('--sessions', nargs=3, required=True, metavar=('ROOT', 'CHILD', 'TARGET'))
parser.add_argument('--native', type=pathlib.Path, required=True)
parser.add_argument('--cli', type=pathlib.Path, required=True)
parser.add_argument('--codex', type=pathlib.Path, required=True)
parser.add_argument('--protected-pid', type=int, action='append', default=[])
args = parser.parse_args()
if sys.platform != 'darwin':
    parser.error('requires the macOS native-smoke executable')
repo = pathlib.Path(__file__).resolve().parents[1]
seed = args.seed_home.resolve()
assert seed.is_relative_to(pathlib.Path('/tmp').resolve()), 'use a disposable seed store under /tmp'
ids = args.sessions
assert len(set(ids)) == 3
cli, native, codex = (p.resolve() for p in (args.cli, args.native, args.codex))
assert all((p.is_file() for p in [cli, native, codex]))
root = pathlib.Path(tempfile.mkdtemp(prefix='fmsmulti-', dir='/tmp'))
(root / 'test-owned').touch()
profile = root / 'codex'
profile.mkdir()
(root / 'home').mkdir()
for sid in ids:
    source = next((seed / 'sessions').rglob('*' + sid + '.jsonl'))
    header = json.loads(source.open().readline())['payload']
    assert header['id'] == sid and header['cwd'].startswith(('/tmp/', '/private/tmp/'))
    if sid != ids[0]:
        assert header.get('forked_from_id') == ids[ids.index(sid) - 1]
    dest = profile / source.relative_to(seed)
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, dest)
(profile / 'config.toml').write_text('cli_auth_credentials_store = "file"\napproval_policy = "on-request"\nsandbox_mode = "read-only"\n[tui]\nscreen_reader_detection_done = true\n')
if (seed / 'models_cache.json').exists():
    shutil.copy2(seed / 'models_cache.json', profile / 'models_cache.json')
env = {k: v for k, v in os.environ.items() if not k.startswith(('FLOWMUX_', 'CODEX_', 'OPENCODE_', 'OPENAI_'))}
env.update(HOME=str(root / 'home'), CODEX_HOME=str(profile), FLOWMUX_SESSION_FLOW_REAL_HOME=str(profile), FLOWMUX_BUNDLED_CLI_PATH=str(cli), FLOWMUX_SESSION_FLOW_MULTI_GUI_DIR=str(root), FLOWMUX_SESSION_FLOW_MULTI_GUI_TARGET=ids[2], GSK_RENDERER='cairo', RUST_BACKTRACE='1', OPENAI_BASE_URL='http://127.0.0.1:9/v1')
env['PATH'] = str(codex.parent) + os.pathsep + env.get('PATH', '')
login = subprocess.run([str(codex), 'login', '--with-api-key'], input='sk-flowmux-session-flow-local-test-not-a-real-key\n', capture_output=True, text=True, env=env, timeout=20)
assert login.returncode == 0
protected = args.protected_pid
for pid in protected:
    os.kill(pid, 0)
processes = {}

def wait_file(name, seconds=75):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        p = root / name
        if p.exists():
            return p
        for role, proc in processes.items():
            if proc.poll() is not None:
                raise RuntimeError(f'{role} exited {proc.returncode}; inspect {root}/{role}.log')
        time.sleep(0.05)
    raise RuntimeError(f'timed out: {name}; test GUIs have bounded internal deadlines; root={root}')
for role, sid in zip(['a', 'b'], ids):
    child_env = env | {'FLOWMUX_SESSION_FLOW_MULTI_GUI_ROLE': role, 'FLOWMUX_SESSION_FLOW_REAL_ID': sid}
    log = (root / f'{role}.log').open('wb')
    p = subprocess.Popen([str(native)], cwd=repo, env=child_env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    processes[role] = p
    print('owned GUI', role, p.pid, 'root', root, flush=True)
    (root / 'driver.json').write_text(json.dumps({'root': str(root), 'pids': {k: v.pid for k, v in processes.items()}, 'profile': str(profile), 'ids': ids}, indent=2))
    wait_file(f'{role}-ready.json')
(root / 'go').touch()
opened = {role: json.loads(wait_file(f'{role}-opened.json').read_text()) for role in processes}
assert sum((v['owns_target'] for v in opened.values())) == 1, opened
assert sorted((v['terminals'] for v in opened.values())) == [1, 2], opened
skew = abs(opened['a']['clicked_at_unix_micros'] - opened['b']['clicked_at_unix_micros']) / 1000
assert skew < 1000, skew
ready = {role: json.loads((root / f'{role}-ready.json').read_text()) for role in processes}
database = ready['a']['database']
assert database == ready['b']['database']
with sqlite3.connect(database) as db:
    operations = db.execute('select request,intent,state,result from operations').fetchall()
    bindings = db.execute('select key,binding from live_sessions').fetchall()
assert len(operations) == 1 and operations[0][2] == 'complete', operations
assert json.loads(operations[0][3])['id'] == ids[2]
ps = subprocess.check_output(['ps', '-axo', 'pid=,ppid=,comm='], text=True)
rows = [r.split(None, 2) for r in ps.splitlines()]
parents = {int(a): int(b) for a, b, _ in rows}
commands = {int(a): c for a, _, c in rows}
owned = {p.pid for p in processes.values()}
while True:
    more = {a for a, b in parents.items() if b in owned}
    if more <= owned:
        break
    owned |= more
writers = {}
for pid in sorted(owned):
    if not commands.get(pid, '').endswith('/codex'):
        continue
    out = subprocess.run(['/usr/sbin/lsof', '-a', '-p', str(pid), '-F0fan'], capture_output=True, timeout=5).stdout
    writable = False
    for field in out.split(b'\x00'):
        field = field.lstrip(b'\n')
        if field.startswith(b'f'):
            writable = False
        elif field in [b'au', b'aw']:
            writable = True
        elif writable and field.startswith(b'n'):
            path = pathlib.Path(os.fsdecode(field[1:]))
            if path.is_relative_to(profile.resolve() / 'sessions') and path.suffix == '.jsonl':
                header = json.loads(path.open().readline())['payload']
                writers.setdefault(header['id'], []).append({'pid': pid, 'path': str(path)})
assert set(writers) == set(ids) and all((len(v) == 1 for v in writers.values())), writers
(root / 'focus-away').touch()
for role in processes:
    wait_file(f'{role}-away.json')
(root / 'refocus').touch()
focus = json.loads(wait_file('foreign-focus-verified.json', 20).read_text())
winner = next((role for role, v in opened.items() if v['owns_target']))
assert focus['owner_pid'] == processes[winner].pid
assert focus['pane'] == opened[winner]['live']['pane'] and focus['surface'] == opened[winner]['live']['surface']
with sqlite3.connect(database) as db:
    assert db.execute('select count(*) from operations').fetchone() == (1,)
for pid in protected:
    os.kill(pid, 0)
result = {'status': 'passed', 'root': str(root), 'source_ids': ids[:2], 'target_id': ids[2], 'ready': ready, 'opened': opened, 'click_start_skew_ms': skew, 'native_writers': writers, 'operations': operations, 'bindings': bindings, 'foreign_focus': focus, 'protected_pids': protected, 'scope': 'Two real native GUI processes and real Codex writers; test controller gestures and IPC focus, not physical pointer or compositor proof. No model request; test-only placeholder credential and loopback endpoint.'}
(root / 'release').touch()
exit_codes = {role: p.wait(timeout=20) for role, p in processes.items()}
assert all((code == 0 for code in exit_codes.values())), exit_codes
result['native_exit_codes'] = exit_codes
(root / 'result.json').write_text(json.dumps(result, indent=2))
for pid in protected:
    os.kill(pid, 0)
print('SESSION_FLOW_MULTI_GUI_REAL_OK', root / 'result.json', 'skew_ms', skew, flush=True)
