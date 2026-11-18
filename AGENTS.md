# lilbox

## All development happens in a worktree

The primary checkout stays on `main`. Never branch, edit, or commit there —
every change gets its own worktree under `.claude/worktrees/`, however small.
That includes one-off fixes, doc edits, config tweaks, and spikes, not just
features.

Create one with the shared bootstrap (it fetches first, branches from the right
base, and symlinks env files):

```bash
.claude/scripts/setup-worktree.sh <branch> --base origin/main --no-install
```

- **`--no-install` is required here.** `runtime.packageManager` is `none` — this
  is a Cargo repo and the script's default install command is `npm install`.
- Branch names follow `vcs.branchConvention` in `.rig/config.json`:
  `{user}/{ticket}-{slug}`.
- The worktree lands at `.claude/worktrees/<last segment of branch>` by default;
  the absolute path is the last line of stdout, so
  `WT=$(.claude/scripts/setup-worktree.sh … | tail -1)`.

Tear it down once its PR merges:

```bash
.claude/scripts/remove-worktree.sh <branch>
```

Teardown skips a dirty worktree rather than failing, so it's safe to run over
all of them. `git worktree list` shows what's currently checked out.

**Commit early.** A worktree's uncommitted edits are lost if anything reaps it,
and reaping is routine here — get work onto its branch rather than leaving it
resident in the working tree.

`/rig-worktree` and the implement-style flows (`/rig-task`, `/rig-epic`,
`/rig-sprint`) already set this up for you. The rule is here for everything
that doesn't go through them.

## Build and test

```bash
cargo test --locked
cargo check --locked
cargo fmt --check
```

CI additionally runs `cargo clippy --locked --all-targets -- -D warnings`, so
run that before pushing.

The live end-to-end paths need a KVM host and network access for image pulls;
`lilbox doctor` must pass on such a host before those can be exercised.

## Repository layout

- `src/` — the `lilbox` binary crate (single binary, `src/main.rs`).
  - `src/commands/` — one module per command group (`new`, `agent`,
    `lifecycle`, `net`, `template`, `view`, `cp`).
  - `src/overlay.rs` — Docker-free OCI image overlay builder (pulls a base
    image, appends a layer, emits an OCI layout tar).
  - `src/tailscale.rs` — tailnet join (ephemeral OAuth-minted keys), serve /
    funnel port management.
  - `src/app.rs` — XDG state/config/data dirs, SQLite state DB, legacy
    `~/.lilbox` migration.
- `templates/` — built-in box templates (Dockerfile + `setup.sh` +
  `template.json` per dir).
- `images/lilbox-box/` — the tailnet-capable base image build.
- `contrib/herdr/` — herdr plugin exposing lilbox as an agent sandbox backend.
- `tests/` — CLI integration tests (pinned to a temp HOME; no live KVM).

## Releasing

```bash
scripts/release.sh patch|minor|major|X.Y.Z [--dry-run]
```

Bumps Cargo.toml + lockfile, commits "Release vX.Y.Z" on a `release/vX.Y.Z`
branch, squash-merges it through a PR (main is protected), then tags
`vX.Y.Z` on the resulting main commit and pushes the tag. The tag triggers
`.github/workflows/release.yml`: binary builds → GitHub Release → crates.io
publish (OIDC trusted publishing; the job refuses tags that don't match
Cargo.toml's version).

Agents **may** run this, but only when the user explicitly asks for a
release — crates.io versions are permanent. The script is self-guarding
(must be on a clean, synced `main`; runs the full fmt/clippy/test gates
before tagging), so `--dry-run` first is the norm when reporting what a
release would do.

## Conventions

- Rust edition 2024, stable toolchain; keep `cargo fmt --check` and clippy
  (`-D warnings`) clean.
- User-facing state lives in XDG dirs (`~/.config/lilbox`,
  `~/.local/state/lilbox`, `~/.local/share/lilbox`) — never the legacy
  `~/.lilbox` except in the one-time migration path.
- Best-effort cleanup paths (tailnet logout, serve teardown, provisioning
  fallout) warn to stderr and must never block the primary operation.
- Secrets reach guests only via microsandbox's secret mechanism or transient
  exec env vars — never through the sandbox builder's persisted config.


<!-- rig:start -->
## Rig

This project uses [Rig](https://github.com/agent-rig/rig) skills, delivered as
standard Agent Skills under `.agents/skills/` — your agent discovers and
invokes them automatically from each skill's trigger description; nothing to
do here to use them. (Claude Code reads the same kit from `.claude/skills/`.)

Project config lives in `.rig/config.json` — read it for the test command,
base branch, tracker, and review-bot settings before running any skill.

**Roles/subagents:** personas live in `.rig/agents/` (rig-reviewer, rig-coder,
rig-architect, rig-qa, rig-debugger). If your agent supports subagents,
delegate to the named persona; otherwise adopt that persona's instructions
inline. Helper scripts are in `.rig/scripts/`; review patterns in
`.rig/REVIEWER.md`; kit reference docs in `.rig/docs/`.

`.rig/` is one rig home for the non-skill pieces: the profile (`config.json`,
`schema.json`) plus symlinks into `.claude/` (`agents/`, `scripts/`, `docs/`,
`REVIEWER.md`, `label-mapping.md`), so there is one source of truth per file.
<!-- rig:end -->
