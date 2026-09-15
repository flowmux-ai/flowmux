<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# SSH workspaces

An SSH workspace uses the local OpenSSH client. Each workspace owns a control
connection; its terminal tabs use separate channels over that connection.
The remote host does not need flowmux installed, but its login shell must
support POSIX shell syntax.

## Connect

Right-click the side panel and choose **New SSH Workspace**, or run:

```sh
flowmux ssh connect user@host --cwd /absolute/remote/path
flowmux ssh connect my-ssh-alias --tmux
```

Host aliases use your OpenSSH configuration. Optional flags include `--port`,
`--user`, `--identity-file`, `--config-file`, and `--name`. The workspace's
**Authentication** button opens the terminal for host-key, password, or MFA
prompts. A successful create response means the workspace exists; inspect
its status to establish that the SSH connection succeeded.

`--tmux` requires tmux on the remote host and keeps each remote tab in a
separate tmux session. Reconnection attaches to those sessions. Without tmux,
disconnecting can terminate remote terminal processes.

A command after `--` runs once in the first tab, followed by a remote shell:

```sh
flowmux ssh connect user@host -- sh -lc 'uname -a'
```

Reconnection does not replay this command. A connection failure does not
prove whether a previously attempted remote command executed.

## Connection and forwarding

Run these commands on the local host, using the SSH workspace UUID from
`flowmux tree`. `--workspace` can be omitted only when the local caller's
`FLOWMUX_WORKSPACE_ID` already identifies that workspace. Remote shells do
not receive flowmux context variables or an installed flowmux CLI.

```sh
flowmux ssh status --workspace WORKSPACE_UUID
flowmux ssh disconnect --workspace WORKSPACE_UUID
flowmux ssh reconnect --workspace WORKSPACE_UUID
flowmux ssh forward add --workspace WORKSPACE_UUID --remote-port 3000
flowmux ssh forward list --workspace WORKSPACE_UUID
flowmux ssh preview FORWARD_UUID --workspace WORKSPACE_UUID
flowmux ssh forward remove FORWARD_UUID --workspace WORKSPACE_UUID
```

Forwards bind to local loopback and target remote loopback. Omit
`--local-port` to allocate a free port; `--https` selects HTTPS for preview.
Preview is supported on Linux and expires when the forward or connection
ends. Workspace configuration survives restart, but connections restore in
the disconnected state and require explicit reconnection.

## Scope

Terminal tabs, splits, connection controls, and explicit port forwarding are
available. Files, Worktrees, editor file I/O, and project commands do not
operate on the remote filesystem. Remote agent hooks and session discovery
are not installed through SSH; visible terminal output is fallback evidence
for remote activity, not an authoritative remote process inventory.
