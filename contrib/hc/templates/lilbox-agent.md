---
description: Coding agent inside a lilbox microVM for the current directory — one box per path, KVM-isolated, real working/blocked/done state over OSC 7501
command: lilbox-hc-agent
---

Run a coding agent inside a lilbox microVM bound to the current directory.
Requires `lilbox` (KVM host), `jq`, and the `lilbox-hc-agent` script from
`lilbox hc install` on PATH. Config via `LILBOX_BIN`, `LILBOX_AGENT_CMD`
(default `claude`), and `LILBOX_IMAGE` (default `python`).

Installed by `lilbox hc install`, which rewrites `command:` to the installed
script's absolute path.
