# lilbox plugin for herdr

Run each [herdr](https://herdr.dev) worktree's coding agent inside its own
lilbox microVM — a real KVM-isolated kernel on hardware you own — and tear the
box down with the worktree.

herdr is an agent multiplexer: persistent workspaces, tabs, panes, and automatic
agent state detection. It runs agents; it doesn't isolate them. lilbox isolates;
it has no multiplexer. This plugin is the seam.

Every other sandbox-backed herdr plugin delegates isolation to someone else's
cloud (E2B, Sprites, Hetzner). This one runs on your box, bind-mounts the live
worktree instead of shuttling snapshots, and costs nothing per hour.

## Requirements

- Linux host with KVM — microsandbox boots libkrun microVMs (`lilbox doctor`)
- `lilbox` on `PATH`
- herdr **0.8.0+** (`pane report-metadata`; popup pane placement)
- `jq` — herdr hands plugins their context as JSON
- `fzf` — for the `lilbox.manage` box picker

## Where to install it (host, not a remote client)

The plugin needs KVM and the `lilbox` binary, so it installs on the herdr
instance **co-located with lilbox** — the Linux/KVM host. This is easy to get
wrong when you drive herdr from another machine:

- **`herdr --remote` (thin client).** One herdr *server*, and it's the remote
  (Linux) one; your local terminal just streams its UI. Plugins already run
  server-side, so install here — on the host.
- **Herdr Mirror.** Two *independent* herdr servers. `herdr-mirror` lives on
  your **local** machine and only SSH-drives the remote (*"the remote needs no
  plugin — just herdr"*). The lilbox plugin is an ordinary remote-side plugin:
  install it on the **remote Linux host's** herdr, and Mirror just projects the
  box pane into your local sidebar. Do **not** install it on the Mirror client
  — no KVM there, and it would find no lilbox state.

Either way, lilbox's keys never cross the boundary. Everything sensitive —
`~/.config/lilbox/config.toml`, Tailscale OAuth / auth keys, the
`ANTHROPIC_API_KEY` the agent pane injects, box SSH — stays on the host where
`lilbox` runs. A remote/Mirror client only ever holds the SSH credential to
reach that host, which it needs regardless of this plugin.

## Install

Run this on the host (see above):

```bash
herdr plugin install lilco-dev/lilbox/contrib/herdr
```

Developing against a checkout instead:

```bash
herdr plugin link /path/to/lilbox/contrib/herdr
herdr plugin action list --plugin lilbox
```

Bind a key by adding to your herdr config:

```toml
[[keys.command]]
key = "prefix+shift+b"
type = "plugin_action"
command = "lilbox.open"
description = "open lilbox"

[[keys.command]]
key = "prefix+shift+a"
type = "plugin_action"
command = "lilbox.agent"
description = "run agent in lilbox"

[[keys.command]]
key = "prefix+shift+m"
type = "plugin_action"
command = "lilbox.manage"
description = "manage lilboxes"
```

### Driving it over Herdr Mirror

Requires **herdr-mirror ≥ v0.2.0**. A `plugin_action` keybinding resolves on
whichever herdr captures the prefix — under Mirror that's your **local**
machine, where lilbox isn't installed — so the bindings above only fire when
set on the host. Mirror's `remote-invoke` bridges that gap: bound locally, it
runs the action on the mirrored host behind your focused pane, handing it that
pane's remote workspace and cwd, and the box pane it opens mirrors back into
your sidebar.

`remote-invoke` takes the action as an argument, which `plugin_action` can't
carry, so bind it as a `shell` command in your **local** herdr config — with
the absolute path, since herdr runs shell bindings through a login `sh` that
never reads your shell rc:

```toml
[[keys.command]]
key = "prefix+alt+a"
type = "shell"
command = "~/.local/bin/herdr-mirror remote-invoke lilbox.agent"
```

Or let Mirror write it for you:

```bash
herdr-mirror remote-actions                    # what each host can invoke
herdr-mirror bind lilbox.agent prefix+alt+a    # write the block + reload herdr
herdr-mirror unbind lilbox.agent               # remove it again
```

Key-bound output is discarded, so failures come back as a toast — plugin not
installed on that host, unreachable host, non-mirrored pane. Note that outside
a mirror `remote-invoke` falls back to running the action on the *local* herdr,
which for lilbox is the machine that deliberately has no plugin: expect a toast,
not a box.

Still available, and the only options on herdr-mirror < 0.2.0:

- **Invoke from a mirrored remote pane.** That shell runs on the host, so
  `herdr plugin action invoke lilbox.agent` hits the remote herdr and its plugin
  with the pane's workspace context. The opened box pane mirrors back to your
  sidebar.
- **Drive the host's herdr directly** with `herdr --remote <host>
  --remote-keybindings server`, so the remote's bindings resolve against the
  remote plugin — at the cost of leaving Mirror's unified view.

The `worktree.removed` teardown hook is unaffected: it's a remote-side event and
fires regardless of how you drive herdr.

## Use

| Action | Does |
|---|---|
| `lilbox.open` | Boot (or reuse) this worktree's box, shell in at `/workspace` |
| `lilbox.agent` | Same box, exec straight into the coding agent |
| `lilbox.boxes` | Live popup of every box (`lilbox ls`) |
| `lilbox.manage` | fzf picker over every box: attach, stop, destroy, expose |
| `lilbox.gc` | Destroy every box whose worktree is gone |
| `lilbox.status` | This worktree's box name, status, and URL |
| `lilbox.expose` | Publish the box over tailnet HTTPS, print the URL |
| `lilbox.unexpose` | Stop publishing |
| `lilbox.kill` | Destroy the box and its home volume |

Invoke any of them with a keybinding, or over the CLI:

```bash
herdr plugin action invoke lilbox.open
```

The `agent` pane is the one that matters for herdr: it runs the agent on a real
PTY inside the microVM, which is what herdr's state detection (working /
blocked / done / idle) reads. The agent edits your actual worktree files —
`/workspace` is a bind mount, not a copy, so there's no sync step in either
direction.

The pane's herdr identity (`HERDR_SOCKET_PATH`, `HERDR_PANE_ID`,
`HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`) is forwarded into the guest at
attach/exec time, as exports in the pane's remote command, so the in-box
agent's herdr hook attributes state to the right pane. (`lilbox agent` also
accepts transient `--env KEY=VAL` pairs, but the pane flow provisions without
a task, so the attach path is the one that carries the identity.) When a
herdr socket is known, the pane also brings up the guest's unix→vsock relay
listener for it. `HERDR_BIN_PATH` is never forwarded: it's a host path,
meaningless inside the box.

Ctrl+click a published `*.ts.net` URL in any pane to shell into the box serving
it.

## Managing boxes: `lilbox.manage`

The manage picker lists **every** box from `lilbox ls` — not just the current
worktree's — so it's the control surface when worktrees are created outside
herdr (driven by agents, say), where the box-per-worktree context actions
don't reach. Keys:

| Key | Does |
|---|---|
| `enter` | attach a shell pane to the box (starting it first if stopped) |
| `ctrl-a` | attach an agent pane instead |
| `ctrl-s` | stop the box — pause it, keep its state |
| `ctrl-x` | destroy the box and its home volume (asks first) |
| `ctrl-e` | toggle tailnet exposure |
| `ctrl-r` | reload the list |

Boxes whose `/workspace` bind-mount source no longer exists on disk are marked
`⚠ path gone`. That's the leak case for agent-driven worktrees: herdr's
`worktree.removed` teardown only fires for worktrees herdr itself manages, so
an agent deleting its own worktree would otherwise orphan a microVM.
`lilbox.gc` destroys every such box non-interactively (`lilbox-herdr gc
--dry-run` to preview) — the path comes from each box's own config, so no
mapping file is involved.

Note the pane itself is just an attach point: closing a box pane detaches your
shell but leaves the box running. Use `ctrl-s` here to pause it or `ctrl-x` to
destroy it.

## One box per worktree

The box name is derived from the worktree's absolute path:
`hd-<slug>-<6 hex of sha256(path)>`. Two consequences worth knowing:

- **Reuse is automatic.** Re-opening the pane attaches to the existing box
  instead of booting a second one; a stopped box is started.
- **Teardown needs no bookkeeping.** The `worktree.removed` hook fires *after*
  the directory is gone, so a mapping file would be the only way to know which
  box to destroy — and a stale one leaks microVMs. Deriving the name from the
  path in the event payload means there's nothing to keep in sync.

The path hash is also what keeps a repo and its `feature/` worktree — same
basename — from colliding onto, and tearing down, each other's box.

## Configuration

`herdr plugin config-dir lilbox` prints the config directory. Drop a
`config.env` there; it's sourced before every command.

```sh
# config.env — all optional, defaults shown
LILBOX_BIN=lilbox          # path to the lilbox binary
LILBOX_IMAGE=python        # image for new boxes; lilbox-box adds tailnet identity
LILBOX_AGENT_CMD=claude    # what the agent pane execs
LILBOX_SHELL=bash          # what the shell pane execs
LILBOX_FZF_BIN=fzf         # picker binary for lilbox.manage
LILBOX_AGENT_ARGS=         # extra `lilbox agent` flags, e.g. --agents-file /path/AGENTS.md
```

Use `LILBOX_IMAGE=lilbox-box` to give every box its own tailnet node — then
`lilbox ssh` is keyless Tailscale SSH and each box gets its own hostname. See
the root README's [`lilbox-box`](../../images/lilbox-box/README.md) section.

The agent needs credentials: `lilbox agent` injects `ANTHROPIC_API_KEY` from
your environment as a scoped secret when it's set, so export it wherever the
herdr server starts.

## Sidebar badge

The plugin reports each pane's box state as a `$lilbox` metadata token on
`pane.created` and `pane.focused`. The value is whatever status word
`lilbox ls` reports (`none` when the worktree has no box). Requires herdr ≥
0.8.0. Surface it in a sidebar row:

```toml
# ~/.config/herdr/config.toml
ui.sidebar.agents.rows = [["state_icon", "workspace", "tab"], ["agent", "$lilbox"]]
```

## Running the shim directly

`bin/lilbox-herdr` works outside herdr, operating on the current directory:

```bash
contrib/herdr/bin/lilbox-herdr name      # box name for $PWD
contrib/herdr/bin/lilbox-herdr status
contrib/herdr/bin/lilbox-herdr open
contrib/herdr/bin/lilbox-herdr manage    # picker works too; attach is local
```

## Tests

`contrib/herdr/tests/run.sh` exercises the shim against stub `lilbox` / `herdr`
/ `fzf` binaries — no KVM host or herdr server needed (needs `bash`, `jq`, and
coreutils). It runs in CI.

Inside herdr it deliberately refuses to fall back to `$PWD` — herdr runs plugin
commands with the *plugin directory* as the working directory, so a fallback
would quietly box up the plugin's own source tree.

## Trust

herdr plugins run as your user with your environment. This one shells out to
`lilbox` and `jq` and calls back into herdr through `HERDR_BIN_PATH`; it is a
manifest and one script, and reading them is the intended way to vet it.

When a box is opened through herdr, the guest receives access to the host's
herdr control socket over vsock — unauthenticated, loopback-equivalent trust —
and agent boxes run with `--dangerously-skip-permissions`. Only open boxes for
repos and agents you would trust with your herdr instance. Boxes created
outside herdr (plain `lilbox new`/`lilbox agent` on the command line) get no
route and cannot reach the socket.
