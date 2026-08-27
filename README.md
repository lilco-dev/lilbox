# lilbox

**Instant, isolated Linux boxes on your own machine — publishable to your
tailnet over HTTPS with one command.**

SSH into a fresh Linux box, run untrusted or AI-generated code behind a real
hardware-virtualization boundary, and publish HTTP endpoints — all on hardware
**you** control. `lilbox` is a single Rust CLI that glues together two
self-hostable pieces:

- [**microsandbox**](https://microsandbox.dev) — libkrun microVMs (a real
  kernel per box, hypervisor-isolated), driven through the embedded Rust SDK.
- [**Tailscale**](https://tailscale.com) — Serve (tailnet HTTPS) / Funnel
  (public) for publishing.

Each box is a genuine microVM — it boots its own Linux kernel in ~1–2s, not a
container sharing yours. `lilbox exec` reports a **different kernel version than
the host**, because it *is* a different kernel.

## How lilbox compares

Most dev sandboxing falls into three buckets — containers, the raw microVM
primitive, or a cloud service. lilbox is the workflow layer that gives you the
last one's ergonomics with the first two's locality:

| | containers (Docker/Podman) | raw microsandbox | cloud sandboxes (E2B, Daytona, …) | **lilbox** |
|---|---|---|---|---|
| Isolation | shared host kernel | hypervisor microVM | vendor VMs | hypervisor microVM (libkrun) |
| Runs on | your host | your host | someone else's cloud | **your host** |
| Your code lives | local | local | round-trips as snapshots | **local — bind-mounts the live worktree** |
| Named persistent devboxes | you script it | — | vendor-specific | built-in (`new` / `fork` / volumes) |
| Publish over HTTPS | wire your own proxy | — | vendor URLs | one command (`expose`) |
| Recurring cost | free | free | per-minute billing | free (your hardware) |
| Agent-in-a-box | DIY | DIY | vendor SDK | `lilbox agent` |

- **vs. containers** — a container shares your kernel; a lilbox box boots its
  own. That hypervisor boundary is the isolation you actually want when running
  code an LLM just wrote — a container escape lands on your host, a microVM
  escape lands in an empty guest kernel.
- **vs. raw microsandbox** — microsandbox is the isolation *primitive*; lilbox
  is the developer *workflow* on top of it: named boxes, templates, persistent
  home volumes, tailnet publishing, TTL/idle lifecycle, fork, and agent-in-a-box
  — none of which you have to script yourself.
- **vs. cloud sandboxes** — E2B, Daytona, Sprites, and friends give you
  sandboxes on *their* hardware and round-trip your code as snapshots. lilbox
  runs on hardware you own and bind-mounts the live worktree, so edits appear on
  your host as they happen — no upload, no per-minute bill, nothing leaving your
  machine.

## Requirements

A host with hardware virtualization — either:

- **Linux** (x86_64 or aarch64) with KVM; nested virtualization is fine, or
- **macOS on Apple Silicon**, which uses Hypervisor.framework in place of KVM

**Intel Macs are not supported.** microsandbox publishes no `darwin-x86_64`
runtime bundle and its HVF backend is aarch64-only, so there is nothing to fall
back to. See [#62](https://github.com/lilco-dev/lilbox/issues/62).

> **macOS is shipped but lightly travelled.** Every release builds an
> `aarch64-apple-darwin` binary against the real HVF backend, but the boot path
> has had far less mileage than the Linux one. If a box fails to come up on a
> Mac, that's a bug worth filing rather than expected behaviour.

Plus:

- Rust 1.85+ and Cargo (build-time only)
- [Tailscale](https://tailscale.com) (only needed for `lilbox expose`)

`lilbox doctor` runs the runtime's host checks for whichever platform you're on.
The Rust SDK downloads its matching runtime components on the first build/use.
The separate `msb` executable is not required.

## Install

```bash
git clone <this repo> && cd lilbox
./install.sh            # builds and installs lilbox into ~/.local/bin
lilbox doctor               # verify the runtime
```

For development, use `cargo run -- <command>`.

### Shell completions

`lilbox completions <shell>` prints a completion script to stdout for `bash`,
`zsh`, `fish`, `elvish`, or `powershell`. Source it from your shell rc:

```bash
# bash — ~/.bashrc
eval "$(lilbox completions bash)"

# zsh — ~/.zshrc (or drop into a dir on $fpath as _lilbox)
eval "$(lilbox completions zsh)"

# fish
lilbox completions fish | source
```

## Where lilbox keeps its state

`lilbox` follows the XDG base directory layout (via the `dirs` crate, so it's
per-OS — paths below are the Linux defaults):

| kind | dir | contents |
|---|---|---|
| config | `~/.config/lilbox/` | `config.toml` |
| state | `~/.local/state/lilbox/` | `state.db`, `logs/` |
| data | `~/.local/share/lilbox/` | `templates/`, `workspaces/` |

Upgrading from an older `lilbox` that used a single `~/.lilbox/` dotdir? The
first run after upgrading moves its contents into the dirs above
automatically (best-effort, with a one-line notice); `~/.lilbox` itself is
left behind, empty.

## Quickstart

```bash
lilbox new web                       # boot a persistent python microVM named "web"
lilbox exec web -- python3 -V        # run a command inside it
lilbox ssh web                       # drop into a shell

# start something listening on the box's port 8000, then:
lilbox expose web                    # -> https://<you>.<tailnet>.ts.net:8443/
lilbox expose web --public           # -> public URL via Tailscale Funnel

lilbox ls                            # see every box + its published URL
lilbox fork web staging              # snapshot + clone (state and all)
lilbox rm web                        # tear down (also unpublishes)
```

Disposable sandbox for a one-off:

```bash
lilbox run -- python3 -c 'print("ran in a throwaway microVM")'
```

## Commands

| Command | Does |
|---|---|
| `lilbox new [NAME] [--template T] [--image I] [--port P] [--cpus N] [--memory M] [--rebuild] [--tailnet\|--tailscale] [--tailnet-tag TAG] [--no-persist] [--volume V] [--ttl D] [--idle-timeout D]` | Boot a persistent box (default image `python`, guest port `8000`, persistent `/root` home; isolated by default — pass `--tailnet` (or `--tailnet-tag`) to join the tailnet) |
| `lilbox templates` | List available box templates |
| `lilbox provision NAME` | Re-run a box's template setup script |
| `lilbox ls [--json]` | List boxes with live status + published URLs (`--json` for scripting) |
| `lilbox gc` | Reap boxes whose `--ttl` has elapsed (cron-friendly) |
| `lilbox exec NAME -- CMD…` | Run a command inside a box |
| `lilbox ssh NAME [-- CMD]` | Interactive shell (or one-shot command) |
| `lilbox cp SRC DST` | Copy files to/from a box (box side = `NAME:/path`) |
| `lilbox logs NAME [-f] [--tail N] [--source S]` | Show or follow captured output |
| `lilbox run [--image I] -- CMD…` | Ephemeral box: boot, run, discard |
| `lilbox rebuild NAME [--image X]` | Recreate a box on a new/updated image, keeping its home volume |
| `lilbox agent [NAME] [--workspace D\|--clone URL] [--agents-file F] -- task` | Run a coding agent (Claude Code) in a box against a mounted workspace |
| `lilbox expose NAME [--public]` | Publish the box over HTTPS (tailnet, or Funnel with `--public`) |
| `lilbox unexpose NAME` | Stop publishing |
| `lilbox join NAME [--tailnet-tag TAG]` | Join a box to the tailnet as its own node (for boxes booted without `--tailnet`, or whose join failed) |
| `lilbox leave NAME` | Log a box out of the tailnet and forget its node |
| `lilbox url NAME` | Print the published URL |
| `lilbox stop/start/restart NAME` | Lifecycle |
| `lilbox fork NAME [NEWNAME] [--force]` | Snapshot a box and boot a clone from it (refuses if the box is on the tailnet — see [forking and tailnet identity](#forking-and-tailnet-identity)) |
| `lilbox volumes` | List persistent home volumes (and orphans) |
| `lilbox image load ARCHIVE --tag TAG` | Import an OCI/Docker archive through the Rust SDK |
| `lilbox image ls` | List the embedded runtime's image cache |
| `lilbox rm NAME [--keep-data]` | Remove a box + unpublish; deletes its home volume unless `--keep-data` |
| `lilbox stat NAME` | Detailed box info |
| `lilbox gateway` | SSH forced-command entry point: dispatch `$SSH_ORIGINAL_COMMAND` through an allowlist ([remote provisioning](#remote-provisioning-over-ssh-lilbox-gateway)) |
| `lilbox doctor` | Check the embedded microsandbox runtime, host virtualization, Tailscale, and tailnet |

## Templates

A **template** is a box that arrives useful — an image + defaults + an optional
post-boot setup script. It's a directory `<name>/`:

```
<name>/
  template.json      # image + defaults (cpus/memory/port)
  setup.sh           # optional: run inside the box after boot (idempotent)
  Dockerfile         # optional: build the image locally instead of pulling
```

```bash
lilbox templates                      # list templates (repo starters + your own)
lilbox new dev --template python-dev   # boot from a template
lilbox provision dev                   # re-run its setup (idempotent)
```

Resolution: user templates in `~/.local/share/lilbox/templates/` **override** repo starters
in `templates/`. Precedence for values is **CLI flag > template > default**
(`lilbox new --image alpine --template python-dev` boots alpine, keeping the
template's other defaults). If a template has a `Dockerfile` (and no `image`),
`lilbox new` builds it with Docker, imports it through `Image::load`, and caches it; `--rebuild`
forces a fresh build. `setup.sh` runs post-boot; a non-zero exit is surfaced and
logged to `~/.local/state/lilbox/logs/<box>-setup.log` (the box is kept for inspection).

Shipped starters: **`python-dev`**, **`node-dev`**, **`rust-dev`**, **`go-dev`**,
**`data-science`** & **`ml-pytorch`** (JupyterLab), **`fullstack-web`** (Node +
Python), **`base-debian`**, and **`agent-sandbox`** — run `lilbox templates` for
the full list, or add your own with `lilbox template add`.

## How publishing works

`lilbox new` maps a guest port to a random host loopback port using
`SandboxBuilder::port`. `lilbox expose` then points
`tailscale serve` at that host port on a dedicated HTTPS port (8443+), so every
box gets its own URL and nothing collides with an existing root `serve`/Caddy
setup. `--public` swaps `serve` for `funnel` (443/8443/10000 only) to reach the
open internet. State (name → image → host port → serve port → URL) lives in
`~/.local/state/lilbox/state.db`.

## Boxes with Tailscale baked in (`lilbox-box`)

The default images (`python`, `alpine`, …) are published from the *host* via
`tailscale serve`. The [`lilbox-box`](images/lilbox-box/) image instead bakes
`tailscale` + `tailscaled` and a first-boot bring-up hook *into the box*, so it
can join your tailnet as its own node — the foundation for per-box hostnames,
keyless `lilbox ssh`, and identity-based auth (epic #2).

```bash
images/lilbox-box/build.sh          # docker build -> lilbox image load
lilbox new mybox --image lilbox-box     # boot it
lilbox exec mybox -- tailscale version  # tailscale is baked in
```

See [`images/lilbox-box/README.md`](images/lilbox-box/README.md) for details.

### `--tailnet`: opt-in tailnet identity on any image

`lilbox new` is **isolation-only by default** — a box never joins your
tailnet just because a credential happens to be sitting in the environment.
Joining is opt-in: pass `--tailnet`, or `--tailnet-tag TAG` (a specific ACL
tag, which implies `--tailnet` on its own), or set `[tailscale] auto = true`
in config for "always join". This supersedes an earlier design where a key
alone in the environment silently triggered a join.

You don't need `lilbox-box` (or a Dockerfile) to get a tailnet-joined box.
When tailnet join is requested and resolves a credential (see below), and the
chosen image isn't already tailnet-capable, `lilbox new` auto-builds a
tailscalified variant of that base via the same docker-free OCI overlay used
by `lilbox image tailscalify` — no local Docker daemon involved. The result
is cached under a tag keyed by the base image and the pinned Tailscale
version (`lilbox/tailnet/<base>-ts<version>`), so the first `lilbox new
--tailnet` against a given base is slower (pulls the base, downloads
Tailscale, builds the overlay) and every repeat is instant. Pass `--rebuild`
to refresh the cached overlay. If the build fails, `lilbox new` warns and
falls back to booting the base image as-is — it never fails because of it.
This means tailnet identity works on any template, not just `lilbox-box`,
with a single flag.

If you ask for `--tailnet` (or `--tailnet-tag`) but no credential resolves,
`lilbox new` prints a note and boots a plain, un-joined box rather than
failing. Conversely, if you don't pass `--tailnet` but the resolved image
already happens to be tailnet-capable (e.g. `--image lilbox-box`), `lilbox
new` prints a note reminding you that `--tailnet` is what actually joins it.

## Tailnet ACLs

Boxes join the tailnet tagged `tag:lilbox-vm`. Before boxes can join and be
reached over Tailscale SSH, add the following to your tailnet's ACL policy:

```jsonc
"tagOwners": { "tag:lilbox-vm": ["autogroup:admin"] },
"ssh": [
  { "action": "accept", "src": ["autogroup:member"], "dst": ["tag:lilbox-vm"], "users": ["root", "autogroup:nonroot"] }
]
```

- `tagOwners` lets an admin identity mint auth keys that carry `tag:lilbox-vm`.
- The `ssh` stanza grants Tailscale SSH from tailnet members to any node
  tagged `tag:lilbox-vm`.

Boxes join as **ephemeral** nodes, so a node auto-deregisters once it goes
offline; `lilbox rm` also logs the node out directly (best-effort) so it's
removed from the tailnet immediately rather than waiting for it to age out.

### Joining after the fact: `lilbox join` / `lilbox leave`

The join folded into `lilbox new` is best-effort by design — a tailnet problem
warns and still leaves you a working box. `lilbox join NAME` is the retry, and
the way to put a box on the tailnet that was never meant to be there at boot:

```bash
lilbox new scratch                 # no tailnet identity
lilbox join scratch                # now its own node, with a MagicDNS name
lilbox leave scratch               # log out, forget the node
```

Both are idempotent. Unlike the join inside `new`, `lilbox join` **fails loudly**
when it can't join — you asked for the tailnet specifically, so quietly carrying
on without it would be a lie.

Boxes that only need to be *reached* (not to have their own identity) don't need
this at all: `lilbox expose` publishes them through the host's tailnet node.

### Forking and tailnet identity

A snapshot captures the guest's `/var/lib/tailscale`, so forking a box that is on
the tailnet would boot a clone presenting the **parent's node key** — two VMs,
one identity, both reachable over Tailscale SSH. `lilbox fork` therefore refuses
to snapshot a joined box:

```bash
lilbox leave web && lilbox fork web    # explicit
lilbox fork web --force                # or leave + fork in one step
```

Clones are scrubbed of inherited tailnet state on boot regardless, which also
covers boxes joined by hand (`lilbox exec … tailscale up`) that lilbox never
recorded. Give a clone its own identity with `lilbox join`.

### Joining the tailnet: OAuth-minted keys vs. a static auth key

Once tailnet join is requested (`--tailnet`, `--tailnet-tag`, or
`[tailscale] auto = true`), `lilbox new` can resolve a credential two ways,
configured under `[tailscale]` in `~/.config/lilbox/config.toml`:

```toml
[tailscale]
tag = "tag:lilbox-vm"                       # optional, defaults to tag:lilbox-vm
oauthClientId = "k123abc..."                 # Tailscale OAuth client ID
oauthClientSecretEnv = "TS_OAUTH_CLIENT_SECRET"  # optional, this is the default
# authKeyEnv = "TS_AUTHKEY"                  # static fallback, see below
# auto = true                                # optional: imply --tailnet on every `lilbox new`
```

- **`oauthClientId` (preferred).** `lilbox new` mints a fresh, tagged,
  single-use, 5-minute-lived auth key per box via the [Tailscale OAuth
  client-credentials flow](https://tailscale.com/kb/1215/oauth-clients),
  reading the client secret from the environment variable named by
  `oauthClientSecretEnv` (default `TS_OAUTH_CLIENT_SECRET`). The OAuth client
  must **own the tag** it mints for (i.e. be scoped to `tag:lilbox-vm` — not
  `lilbox`, the tag prefix is required) and hold the `auth_keys` write scope.
  Nothing is cached: a new key is minted for every `lilbox new`.
- **`authKeyEnv` (static fallback).** Names an environment variable holding a
  long-lived, pre-generated auth key (default `TS_AUTHKEY`). Used only when
  `oauthClientId` isn't configured, or its secret env doesn't resolve.

**Precedence:** OAuth (`oauthClientId` + resolvable secret) > `authKeyEnv` >
skip the tailnet join entirely. Any minting failure (bad credentials, network
error, malformed response) only prints a warning and skips the join — it
never fails `lilbox new`.

**Security note:** joining the tailnet requires `tailscaled` in the guest to
run `tailscale up --auth-key=...` itself — there's no way to join a node from
the host on its behalf — so the box necessarily receives its own real auth
key. It's delivered only as a transient environment variable on the one
`tailscale up` exec, never through `lilbox`'s builder-level secret mechanism,
so it's never written into the sandbox's persisted config at rest. Prefer
**ephemeral, single-use** keys (the OAuth-minted path above already mints
these) so the guest's momentary visibility of its own key is moot.

## Quickstart: a box on your tailnet

Prereqs (once): the [Tailnet ACLs](#tailnet-acls) in place and a Tailscale
credential. Any image works — pass `--tailnet` and `lilbox new`
[auto-builds a tailnet-capable
variant](#--tailnet-opt-in-tailnet-identity-on-any-image) of whatever base
you pick (cached after the first run).

1. Pick a credential — OAuth (recommended: mints a fresh, 5-minute key per
   box) via `oauthClientId` in `[tailscale]` plus
   `export TS_OAUTH_CLIENT_SECRET=...`; or a static, ephemeral,
   pre-authorized `tag:lilbox-vm` key via `export TS_AUTHKEY=tskey-auth-...`.
   See [OAuth-minted keys vs. a static auth
   key](#joining-the-tailnet-oauth-minted-keys-vs-a-static-auth-key) for the
   tradeoffs.
2. `lilbox new dev --tailnet` — the first run builds and caches a
   tailscalified image; repeats (with the same base) are instant. (Set
   `[tailscale] auto = true` if you want every `lilbox new` to join without
   passing the flag.)
3. Use it:
   - `lilbox url dev` → `https://dev.<tailnet>.ts.net/`, serving the app on
     guest port 8000
   - `lilbox ssh dev` — Tailscale SSH, keyless
4. `lilbox rm dev` — deregisters the node.

Troubleshooting: a `could not join tailnet` warning is almost always a
missing/rejected credential, or an auto-tailscalify build failure (printed as
a separate warning; `lilbox new` still boots the base image, just without a
tailnet join). A plain `lilbox new` (no `--tailnet`/`--tailnet-tag`/`auto`)
never attempts a join, even if a credential is present in the environment.

## Remote provisioning over SSH (`lilbox gateway`)

Run boxes on a KVM host from a laptop that has nothing but `ssh` — the exe.dev
move:

```bash
ssh -t internal-host new --tailscale            # create a box and land in its shell
ssh -t internal-host new --tailscale --image node my-box
ssh internal-host ls
ssh internal-host rm my-box
```

The mechanism is an **SSH forced command**. `sshd` puts whatever the client
typed after the host into `$SSH_ORIGINAL_COMMAND` and runs a fixed program;
`lilbox gateway` reads that string, parses it with the *same* CLI grammar, and
dispatches it through a strict allowlist. No daemon, no open port, no local
lilbox install.

**Grammar is identical to the local CLI**, so the positional is the **box
name**, not the image: `new --tailscale python` makes a box *named* `python`
running the gateway's default image. Choose the runtime with `--image`
(`new --tailscale --image node`), and set the default for bare `new` with a
`[gateway]` table in the host's `~/.config/lilbox/config.toml`:

```toml
[gateway]
image = "python"   # booted when `new` is given neither --image nor a template
```

Image precedence on the gateway path: `--image` › template image › `[gateway]
image` › `[image]` › built-in default.

**Attach UX:** with `ssh -t` (a PTY) a gateway `new` drops you straight into the
new box's shell; exiting closes the SSH session but leaves the box running.
Without a TTY (`ssh host new ... < /dev/null` in a script) it prints the box's
name and MagicDNS URL instead.

### Host setup

Give a dedicated user on the host the KVM stack, `lilbox` on `PATH`, and an
`authorized_keys` entry per client key that pins the forced command:

```
command="/usr/local/bin/lilbox-gateway",no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-pty ssh-ed25519 AAAA... you@laptop
```

(Drop `no-pty` — or use `command="lilbox gateway"` directly — if you want the
attach-into-the-box UX, which needs a PTY.)

Forced commands run with a **minimal environment**, so a bare `command="lilbox
gateway"` won't see the Tailscale OAuth secret. Point it at a thin wrapper that
exports the credential first:

```sh
#!/bin/sh
# /usr/local/bin/lilbox-gateway
export TS_OAUTH_CLIENT_SECRET="tskey-client-..."   # or TS_AUTHKEY=...
exec lilbox gateway
```

**Allowlist.** Only `new`, `ssh`, `ls`, `stat`, `url`, `logs`, `stop`, `start`,
`restart`, `rm` are reachable; everything else (`cp`, `exec`, `run`, `agent`,
image/template management, `gateway` itself) is rejected. Because the command is
`shlex`-split into an argv and handed straight to the parser — never to a shell
— `ssh host 'new; rm -rf /'` is inert: `new;` is just an unknown subcommand.
Note that `rm` lets a key holder delete boxes and `ssh <box> -- <cmd>` runs
commands *inside* a box, so treat each authorized key as trusted.

### Heads-up: Tailscale SSH bypasses this

If the host runs **Tailscale SSH** (`tailscale up --ssh`), tailnet connections
to port 22 are handled by `tailscaled`, which does **not** read OpenSSH's
`authorized_keys` — so the forced-command entry above never fires over the
tailnet, and every connection lands in the target user's normal login shell.
The forced-command gateway therefore only works where **OpenSSH** actually
handles the connection: a host without Tailscale SSH on :22, an OpenSSH instance
on a non-22 port (Tailscale SSH only intercepts 22), or a LAN/loopback path.

A Tailscale-SSH-native gateway — a dedicated login-shell user gated by a tailnet
`ssh` policy stanza, with per-caller identity from `tailscale whois` (no keys) —
is the intended path on such hosts and is tracked as follow-up.

## Persistent volumes (devboxes)

Every `lilbox new` box gets a named microsandbox volume `lilbox-<name>-home` mounted at
`/root`, so its home **survives `lilbox stop`/`start` and image swaps** — a sandbox
becomes a devbox. Pass `--no-persist` for a throwaway home (`lilbox run` is always
ephemeral).

```bash
lilbox new dev                     # persistent /root by default
lilbox rebuild dev --image python  # swap the image, keep the home data
lilbox rm dev --keep-data          # remove the box but preserve its volume
lilbox new dev                     # a same-named box re-attaches it
lilbox new dev2 --volume lilbox-dev-home   # or adopt it explicitly by name
lilbox volumes                     # list volumes (orphans flagged)
```

`lilbox rm` **deletes the home volume by default** (unless `--keep-data`, or another
box still uses it). `lilbox rebuild` never touches the volume — on a boot failure the
data is preserved and a bare `lilbox rebuild NAME` retries.

## Lifecycle & config (cheap when idle)

- **TTL:** `lilbox new web --ttl 2h` auto-expires the box; `lilbox gc` (run it from cron)
  reaps expired boxes. `lilbox run --ttl 30m` bounds an ephemeral box.
- **Idle suspend:** `lilbox new web --idle-timeout 20m` suspends the box after it's
  idle (frees RAM); `lilbox exec`/`lilbox ssh` **transparently resume** it (~1–2s). A box
  you `lilbox stop` yourself is never auto-resumed.
- **Defaults & caps:** `~/.config/lilbox/config.toml` sets defaults for
  `image`/`port`/`cpus`/`memory`/`ttl`/`idle_timeout` and caps
  (`max_cpus`/`max_memory`) that clamp an over-limit `lilbox new`. Precedence: CLI
  flag > template > config > built-in. See
  [`config.toml.example`](config.toml.example).

## Agent-in-a-box

`lilbox agent` runs a coding agent (Claude Code) **inside** a microVM against a
mounted workspace — safe by construction, since AI-generated code runs behind
the hypervisor boundary, not on your host.

```bash
lilbox agent -- "add a test for the parser"        # agent works in the current dir
lilbox agent --clone https://github.com/you/repo -- "fix the failing CI"
lilbox agent --agents-file ./AGENTS.md -- "..."     # pass repo agent instructions
```

The workspace is bind-mounted at `/workspace`, so the agent's edits appear on
the host **as it works** — retrieve them directly (or `lilbox expose` what it built).
Bring your own key: `export ANTHROPIC_API_KEY=…` and it's injected via
the SDK's `secret_env` API — sent only to `api.anthropic.com`
and **never written into the box's image or state**. Override with
`--key-env`/`--key-host`.

## Use it from herdr

[herdr](https://herdr.dev) is an agent multiplexer — persistent workspaces,
panes, and automatic agent state detection. It runs agents but doesn't isolate
them; lilbox isolates but has no multiplexer.
[`contrib/herdr/`](contrib/herdr/) is the seam: a herdr plugin that gives every
worktree its own microVM and destroys it with the worktree.

```bash
herdr plugin install lilco-dev/lilbox/contrib/herdr   # on the lilbox host
herdr plugin action invoke lilbox.agent    # boot this worktree's box, run the agent in it
```

Every other sandbox-backed herdr plugin delegates isolation to someone else's
cloud. This one runs on your hardware and bind-mounts the live worktree instead
of shuttling snapshots. Install it on the **lilbox host**, not a `herdr --remote`
or Herdr Mirror client — see
[`contrib/herdr/README.md`](contrib/herdr/README.md) for the topology and config.

## Configuration

`~/.config/lilbox/config.toml` sets `lilbox new` defaults using standard TOML. Precedence
is **CLI flag > template > config > built-in default**. See
[`config.toml.example`](config.toml.example).

## Testing

Run the Rust unit tests and compile checks with Cargo:

```bash
cargo test --locked
cargo check --locked
```

The live end-to-end smoke test needs a KVM host and network access for image
pulls.

## What this POC does *not* do (yet)

- **No auth on published endpoints** beyond what your tailnet already enforces.
  `--public` is genuinely public — anything the box serves is on the internet.
- **One published web port per box.** Multi-port / arbitrary later ports would
  mean re-creating the box (microsandbox fixes published ports at boot).
- **No custom domains / per-box DNS names.** Boxes are distinguished by HTTPS
  port, not hostname (a Caddy vhost layer could add this).
- **Not multi-tenant.** Single user, single host.

## License

MIT
