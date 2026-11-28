# lilbox template for herdr-core (hc)

[herdr-core](https://herdr.dev) (`hc`) runs coding agents in daemon-owned
terminals and reads their state from OSC 7501 self-reports. lilbox's attach
bridge passes those escapes through byte-transparently, so an agent running
inside a microVM is a first-class hc citizen — real `working` / `blocked` /
`done` states, turn counting, `hc wait` gating — with no plugin and no
screen-scraping. This directory replaces the retired `contrib/herdr` plugin.

## Install

```bash
lilbox hc install                                       # local flavor
lilbox hc install --remote-host lilbox@<box-host>       # + remote flavor
```

Then, from any directory you want to box up:

```bash
hc run lilbox-agent "fix the failing tests"    # box on this host (needs KVM)
hc run lilbox-remote "fix the failing tests"   # box on the gateway host
```

Both flavors share one entrypoint (`bin/lilbox-hc-agent`), switched by
`LILBOX_SSH_HOST`: unset boots locally with `lilbox agent`; set boots over the
[lilbox SSH gateway](../../README.md#remote-provisioning-over-ssh-lilbox-gateway)
with plain `ssh` — no local KVM or lilbox install needed, so any machine with
gateway access can drive box-agents. Note the gateway's boxes belong to the
gateway user on that host, not to your login there.

## How it works

`hc run lilbox-agent [task]` starts the installed entrypoint
(`bin/lilbox-hc-agent`) in a terminal with the current directory as its cwd,
then pastes the task into its stdin. The entrypoint:

1. Derives the box name from the canonical path: `hd-<slug>-<6 hex of
   sha256(path)>`. Same scheme as the retired herdr plugin, so old boxes are
   reused rather than orphaned; the hash keeps same-basename worktrees from
   colliding.
2. Boots the box if it's missing (`lilbox agent --workspace <path>` — the
   directory is bind-mounted at `/workspace`), starts it if it's stopped, and
   recreates it if its record is stale (`gone`).
3. `exec lilbox attach <box> -- sh -lc 'cd /workspace; exec <agent>'` — a raw PTY
   bridge, so the agent's OSC 7501 escapes reach the hc core and the pasted
   task reaches the agent.

The remote flavor swaps those steps for gateway calls: `ssh $LILBOX_SSH_HOST
new/ls/start/rm` for lifecycle, an idempotent agent install exec (persists in
the box's home volume), and `ssh -tt $LILBOX_SSH_HOST "attach <box> -- ..."`
for the PTY. Remote boxes have no `/workspace` mount — the agent works on a
checkout inside the box (clone it there, exe.dev-style) — and authenticate
once per box (`/login` persists in the home volume); the gateway has no
credential-injection path.

The box stays up between runs; `lilbox stop <box>` when you're done.

## Configuration

Environment variables, read by the entrypoint at run time:

| Var | Default | Meaning |
|---|---|---|
| `LILBOX_BIN` | `lilbox` | path to the lilbox binary (local flavor) |
| `LILBOX_AGENT_CMD` | `claude` | agent exec'd inside the box |
| `LILBOX_IMAGE` | `python` | image for newly created boxes |
| `LILBOX_SSH_HOST` | unset | gateway target (`user@host`); set = remote flavor (baked into the `lilbox-remote` template at install time) |

Requires `jq` (box status lookup). `lilbox hc install` must be re-run after
upgrading lilbox to pick up entrypoint changes.

## Files

- `bin/lilbox-hc-agent` — the entrypoint. Review copy of what `lilbox hc
  install` embeds; `tests/hc_template.rs` pins the installed copy to this one.
- `templates/lilbox-agent.md` / `templates/lilbox-remote.md` — the templates
  as installed (the installer rewrites `command:` with the installed
  entrypoint's absolute path and, for the remote flavor, the gateway host).
