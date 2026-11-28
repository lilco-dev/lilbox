---
description: Coding agent inside a lilbox microVM on a remote gateway host — one box per directory, no local KVM needed, real working/blocked/done state over OSC 7501
command: LILBOX_SSH_HOST=lilbox@box-host lilbox-hc-agent
---

Run a coding agent inside a lilbox microVM on the lilbox SSH gateway host,
bound to the current directory by name. Requires `ssh` access to the gateway
and `jq`; the gateway host needs KVM and lilbox. The box's home volume
persists agent installs and logins across runs.

Installed by `lilbox hc install --remote-host <user@host>`, which rewrites
`command:` with the real gateway target and the installed script's absolute
path.
