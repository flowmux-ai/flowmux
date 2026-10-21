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

**New SSH Workspace** prefills the last successful connection's host/user,
port, remote directory, workspace name, identity-file path, SSH-config path,
and tmux choice. Only a successful OpenSSH control connection updates these
defaults; failed or cancelled attempts do not. Port forwards and one-shot
commands are not copied. Loading defaults never overwrites edits already
made in the dialog.

The defaults persist in the OS credential store: Secret Service on Linux
(for example GNOME Keyring or KWallet), and Keychain on macOS. Use a
password-protected persistent keyring for encryption at rest; Flowmux also
encrypts Secret Service transport. Different Flowmux state directories use
separate entries. Connection details are stored in the secret payload, not
searchable keyring attributes. No passwords, private-key contents, or MFA
answers are collected. If the keyring is unavailable, Flowmux reports that
history could not be loaded/saved and does not create a plaintext fallback.

This protects the **autofill history**. Existing workspace restoration data
in `state.json` still includes SSH configuration and remote paths in plaintext;
this feature does not encrypt workspace state, review drafts, or terminal logs.

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
Preview is supported on Linux and macOS and expires when the forward or connection
ends. Workspace configuration survives restart, but connections restore in
the disconnected state and require explicit reconnection.

## Code Review

Open **Code Review** from the pane menu or press **Ctrl+Alt+E** in a connected
SSH workspace. Flowmux reads the remote checkout's working changes (including
staged and untracked files), commit history, and selected commit diffs through
the existing SSH connection. Git must be available on the remote host; no
remote Flowmux installation or local clone is needed.

The review follows the terminal tab's reported remote directory, falling back
to the workspace directory. Review comments are saved locally, separately for
each SSH target and checkout. Git reads do not modify the remote checkout or
staging area. After a disconnection, reconnect the workspace and reopen or
refresh the review. Reads never start a separate authentication session.

To send comments, run Codex (or another supported agent) in a terminal in the
same SSH workspace, save the comments, and select the agent under **Send**.
The agent must be idle with an empty prompt. Flowmux sends the feedback through
that terminal's existing SSH channel; remote Flowmux hooks are not required.
Disconnecting or replacing the channel invalidates an open Send selection.
Remote agent detection uses terminal screen/title evidence, so it cannot verify
the agent's exact process or session identity. If detection is unavailable,
use **Copy feedback** and paste into the intended agent yourself.

## Scope

Terminal tabs, splits, Code Review, connection controls, and explicit port forwarding are
available. Files, Worktrees, editor file I/O, and project commands do not
operate on the remote filesystem. Remote agent hooks and session discovery
are not installed through SSH; visible terminal output is fallback evidence
for remote activity, not an authoritative remote process inventory.
