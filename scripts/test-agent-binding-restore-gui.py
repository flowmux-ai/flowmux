#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
from agent_host_protocol import SOCKET_NAME

import argparse
import copy
import fcntl
import importlib.util
import json
from pathlib import Path
import sys
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent

def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / filename)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value

gui = module('gui', 'test-ssh-workspace-gui.py')
host = module('host', 'test-agent-host-gui.py')
parser = argparse.ArgumentParser(description="Verify authoritative surface ownership across crash-before-save restores")
parser.add_argument('--gui', default='target/debug/flowmux')
parser.add_argument('--cli', default='target/debug/flowmuxctl')
parser.add_argument('--host', default='target/debug/flowmux-daemon')
parser.add_argument('--background', action='store_true', help='detach the winner before restoring the stale source')
parser.add_argument('--protected-pid', type=int, action='append', default=[])
args = parser.parse_args()
for name in ('gui', 'cli', 'host'):
    setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
h = gui.Harness(args)
control = None
try:
    options = h.root / 'config/flowmux/options.json'
    options.parent.mkdir(parents=True, exist_ok=True)
    options.write_text(json.dumps({'default_shell': '/bin/sh'}))
    h.start_display()
    host_socket = h.root / "run" / SOCKET_NAME
    with (h.root / 'host.log').open('w') as log:
        h.spawn([args.host, '--agent-host', '--socket', str(host_socket)],
                stdout=log, stderr=log)
    gui.wait_for(host_socket.exists, 'isolated Host')
    control = host.HostClient(host_socket, 'relay')
    agent = control.call('launch', launch={
        'request_id': host.launch_request_id(), 'provider': 'fixture', 'label': 'Restore audit',
        'argv': ['/bin/sh', '-c', 'printf \'%s\\n\' "$$" >> "$FIXTURE_STARTS"; stty -echo; printf \'AUDIT-READY\\n\'; while IFS= read -r line; do printf \'ACK:%s\\n\' "$line"; done'],
        'cwd': str(h.root), 'env': list(h.env.items()) + [('FIXTURE_STARTS', str(h.root / 'starts'))], 'size': {'rows': 24, 'cols': 80},
    })['agent']

    def current():
        return next(item for item in control.call('list')['agents'] if item['instance'] == agent['instance'])

    def exact_execution():
        value = current()
        assert value['state'] == 'running'
        assert (value['pid'], value['process_start']) == (agent['pid'], agent['process_start'])
        assert (h.root / 'starts').read_text().splitlines() == [str(agent['pid'])]
        assert int(Path(f"/proc/{agent['pid']}/stat").read_text().rsplit(') ', 1)[1].split()[19]) == agent['process_start']

    first, first_sock = h.window('restore-audit-source')
    workspace = h.rpc(first_sock, 'workspace_create', root=str(h.root), name='Restore audit source')['workspace_created']['id']
    h.rpc(first_sock, 'workspace_focus', workspace=workspace)
    pane = h.workspace(first_sock, workspace)['panes'][0]['id']
    plain_surface = h.workspace(first_sock, workspace)['panes'][0]['tabs'][0]['id']
    original_surface = h.rpc(first_sock, 'agent_view_open', pane=pane, instance=agent['instance'])['surface_created']['id']
    gui.wait_for(lambda: current()['view'] and current()['view']['pane'] == pane, 'source reported ready')
    gui.wait_for(lambda: h.persisted(workspace, [original_surface]), 'source binding saved')
    saved = json.loads(h.state_path.read_text())
    stale_workspace = next(ws for ws in saved['workspaces'] if ws['id'] == workspace)
    stale_owner = saved['workspace_owners'][workspace]
    stale_window = next(window for window in saved['windows'] if window['instance_id'] == stale_owner)
    second, second_sock = h.window('restore-audit-current')
    second_ws = h.rpc(second_sock, 'workspace_create', root=str(h.root), name='Current owner')['workspace_created']['id']
    h.rpc(second_sock, 'workspace_focus', workspace=second_ws)
    second_pane = h.workspace(second_sock, second_ws)['panes'][0]['id']
    second_surface = h.rpc(second_sock, 'agent_view_open', pane=second_pane, instance=agent['instance'])['surface_created']['id']
    gui.wait_for(lambda: current()['view'] and current()['view']['pane'] == second_pane, 'transferred ready owner')
    gui.wait_for(lambda: all(tab['id'] != original_surface for ws in h.tree(first_sock) for item in ws['panes'] for tab in item['tabs']), 'old view removed')
    gui.wait_for(lambda: h.persisted(second_ws, [second_surface]), 'destination saved')
    committed = current()['attachment']['key']
    authority = h.state_path.with_suffix(f'.agent-views.{agent["epoch"]}.json')
    assert json.loads(authority.read_text())[agent['instance']] == second_surface
    if args.background:
        h.rpc(second_sock, 'surface_close', pane=second_pane, surface=second_surface)
        gui.wait_for(lambda: current()['attachment'] is None, 'winner detached')
        assert current()['view_owner']['surface'] == second_surface
    # Only our isolated process is killed. Recreate the source's crash-before-save
    # disk slice under the product's state lock, preserving the current window.
    assert first in h.children and first.pid not in h.protected_pids
    first.kill()
    first.wait(timeout=10)
    with h.state_path.with_name('state.lock').open('a+') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        disk = json.loads(h.state_path.read_text())
        disk['workspaces'] = [ws for ws in disk['workspaces'] if ws['id'] != workspace]
        disk['workspaces'].append(copy.deepcopy(stale_workspace))
        if workspace not in disk['workspace_order']:
            disk['workspace_order'].append(workspace)
        disk['workspace_owners'][workspace] = stale_owner
        disk['windows'] = [item for item in disk['windows'] if item['instance_id'] != stale_owner]
        disk['windows'].append(copy.deepcopy(stale_window))
        temporary = h.state_path.with_suffix('.audit.tmp')
        temporary.write_text(json.dumps(disk))
        temporary.replace(h.state_path)
    # A later save by another GUI must remove the crashed source's obsolete slice
    # without waiting for that source to start or query the Host.
    h.rpc(second_sock, 'workspace_create', root=str(h.root), name='Offline cleanup trigger')
    gui.wait_for(lambda: original_surface not in h.state_path.read_text(), 'offline source binding reconciled on another window save')
    assert json.loads(authority.read_text())[agent['instance']] == second_surface
    h.pass_check('A Host admission is durable and another window save removes the offline source binding before restore')
    restored, restored_sock = h.window('restore-audit-stale')
    h.rpc(restored_sock, 'workspace_focus', workspace=workspace)
    def source_tabs():
        return [tab for ws in h.tree(restored_sock) for item in ws['panes'] for tab in item['tabs']]
    gui.wait_for(lambda: all(tab['id'] != original_surface for tab in source_tabs()), 'obsolete restored tab removed')
    # A previously saved ordinary terminal must survive the managed-tab cleanup.
    original_tabs = [tab['id'] for item in h.workspace(restored_sock, workspace)['panes'] for tab in item['tabs']]
    assert original_tabs == [plain_surface], original_tabs
    gui.wait_for(lambda: original_surface not in h.state_path.read_text(), 'obsolete saved binding removed')
    assert current()['view_owner']['surface'] == second_surface
    if args.background:
        assert current()['attachment'] is None, 'stale restore acquired Background execution'
    else:
        assert current()['attachment']['key'] == committed
        h.send(second_sock, second_pane, 'after-stale-restore')
        gui.wait_for(lambda: 'ACK:after-stale-restore' in h.screen(second_sock, second_pane), 'current owner input after stale restore')
    exact_execution()
    h.pass_check('Obsolete restored tab and saved binding are removed while preserving ordinary tabs, current ownership and exact process')
    # Explicit recovery creates a new surface after automatic stale binding removal.
    recovery_ws = h.rpc(restored_sock, 'workspace_create', root=str(h.root), name='Recovery target')['workspace_created']['id']
    h.rpc(restored_sock, 'workspace_focus', workspace=recovery_ws)
    recovery_pane = h.workspace(restored_sock, recovery_ws)['panes'][0]['id']
    recovered = h.rpc(restored_sock, 'agent_view_open', pane=recovery_pane, instance=agent['instance'])
    recovery_surface = recovered['surface_created']['id']
    assert recovery_surface != original_surface
    gui.wait_for(lambda: current()['view'] and current()['view']['pane'] == recovery_pane, 'explicit recovery location')
    h.send(restored_sock, recovery_pane, 'recovered-stale-binding')
    gui.wait_for(lambda: 'ACK:recovered-stale-binding' in h.screen(restored_sock, recovery_pane), 'recovered real VTE input')
    exact_execution()
    h.pass_check('Explicit open from Agents admits a new surface with working input and exactly one provider spawn')
    gui.wait_for(lambda: h.persisted(recovery_ws, [recovery_surface]), 'winning binding saved')
    # A real crash of the winner must restore the same surviving provider and SurfaceId.
    assert restored in h.children and restored.pid not in h.protected_pids
    restored.kill()
    restored.wait(timeout=10)
    gui.wait_for(lambda: current()['attachment'] is None, 'crashed winner relay detached')
    restored, restored_sock = h.window('restore-audit-winner-recovered')
    h.rpc(restored_sock, 'workspace_focus', workspace=recovery_ws)
    gui.wait_for(lambda: current()['view'] and current()['view']['surface'] == recovery_surface, 'winning binding reconnects automatically')
    h.send(restored_sock, recovery_pane, 'after-winner-crash')
    gui.wait_for(lambda: 'ACK:after-winner-crash' in h.screen(restored_sock, recovery_pane), 'restored winner real VTE input')
    exact_execution()
    h.pass_check('The winning saved surface automatically reconnects after GUI SIGKILL with the same process, snapshot and fresh input')
    h.rpc(restored_sock, 'surface_close', pane=recovery_pane, surface=recovery_surface)
    gui.wait_for(lambda: current()['attachment'] is None, 'closed recovered view Background')
    exact_execution()
    h.pass_check('Closing the recovered tab preserves the exact execution in Background')
finally:
    try:
        if control:
            for item in control.call('list')['agents']:
                if item['state'] == 'running':
                    control.call('terminate', instance=item['instance'], epoch=item['epoch'], mode='terminate')
                    gui.wait_for(lambda: not Path(f"/proc/{item['pid']}").exists(), 'audit execution reaped', timeout=10)
    finally:
        if control:
            control.close()
        h.cleanup()
