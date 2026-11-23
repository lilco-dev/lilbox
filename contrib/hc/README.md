# lilbox template for herdr-core (hc)

[herdr-core](https://herdr.dev) (`hc`) runs coding agents in daemon-owned
terminals and reads their state from OSC 7501 self-reports. lilbox's attach
bridge passes those escapes through byte-transparently, so an agent running
inside a microVM is a first-class hc citizen — real `working` / `blocked` /
`done` states, turn counting, `hc wait` gating — with no plugin and no
screen-scraping. This directory replaces the retired `contrib/herdr` plugin.

## Install

```bash
lilbox hc install        # writes the template + entrypoint for this user
```

Then, from any directory you want to box up:

```bash
hc run lilbox-agent "fix the failing tests"
```

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
3. `exec lilbox ssh <box> -- sh -lc 'cd /workspace; exec <agent>'` — a raw PTY
   bridge, so the agent's OSC 7501 escapes reach the hc core and the pasted
   task reaches the agent.

The box stays up between runs; `lilbox stop <box>` when you're done.

## Configuration

Environment variables, read by the entrypoint at run time:

| Var | Default | Meaning |
|---|---|---|
| `LILBOX_BIN` | `lilbox` | path to the lilbox binary |
| `LILBOX_AGENT_CMD` | `claude` | agent exec'd inside the box |
| `LILBOX_IMAGE` | `python` | image for newly created boxes |

Requires `jq` (box status lookup). `lilbox hc install` must be re-run after
upgrading lilbox to pick up entrypoint changes.

## Files

- `bin/lilbox-hc-agent` — the entrypoint. Review copy of what `lilbox hc
  install` embeds; `tests/hc_template.rs` pins the installed copy to this one.
- `templates/lilbox-agent.md` — the template as installed (the installer
  rewrites `command:` to the installed entrypoint's absolute path).
