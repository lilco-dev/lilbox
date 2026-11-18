#!/usr/bin/env bash
# release.sh — cut a lilbox release: bump version, commit, tag, push.
#
#   scripts/release.sh patch|minor|major|X.Y.Z [--dry-run]
#
# The tag push triggers .github/workflows/release.yml: binary builds, the
# GitHub Release, and the crates.io publish (which refuses the tag if it
# doesn't match Cargo.toml's version). Published crate versions are
# permanent — this script front-loads every check so a bad release can't
# get that far.
set -euo pipefail

usage() { sed -n "2,10p" "$0"; exit "${1:-0}"; }

BUMP=""
DRY=0
for arg in "$@"; do
  case "$arg" in
    patch|minor|major) BUMP="$arg" ;;
    --dry-run) DRY=1 ;;
    -h|--help) usage ;;
    [0-9]*.[0-9]*.[0-9]*) BUMP="$arg" ;;
    *) echo "unknown argument: $arg" >&2; usage 1 ;;
  esac
done
[ -n "$BUMP" ] || { echo "missing version bump (patch|minor|major|X.Y.Z)" >&2; usage 1; }

cd "$(git rev-parse --show-toplevel)"

echo "== preflight =="

branch="$(git branch --show-current)"
[ "$branch" = "main" ] || { echo "not on main (on '$branch')" >&2; exit 1; }

git diff --quiet && git diff --cached --quiet || { echo "working tree is dirty" >&2; exit 1; }

git fetch origin main --quiet
[ "$(git rev-parse main)" = "$(git rev-parse origin/main)" ] || {
  echo "local main != origin/main — push or rebase first" >&2; exit 1;
}

current="$(cargo metadata --no-deps --format-version 1 | sed -E 's/.*"version":"([0-9]+\.[0-9]+\.[0-9]+)".*/\1/')"
case "$BUMP" in
  patch|minor|major)
    IFS=. read -r major minor patch <<< "$current"
    case "$BUMP" in
      patch) patch=$((patch + 1)) ;;
      minor) minor=$((minor + 1)); patch=0 ;;
      major) major=$((major + 1)); minor=0; patch=0 ;;
    esac
    next="$major.$minor.$patch"
    ;;
  *) next="$BUMP" ;;
esac

[ "$next" != "$current" ] || { echo "version $next is already current" >&2; exit 1; }
git rev-parse -q --verify "refs/tags/v$next" >/dev/null && {
  echo "tag v$next already exists" >&2; exit 1;
}

echo "  $current -> $next"

echo "== gates =="
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked

if [ "$DRY" = 1 ]; then
  echo "== dry run: would bump to $next, commit 'Release v$next', tag v$next, push =="
  exit 0
fi

echo "== release v$next =="

# Bump Cargo.toml (first version key is [package]'s) and let cargo refresh
# the lockfile's entry for the workspace package.
sed -i "0,/^version = \"$current\"$/s//version = \"$next\"/" Cargo.toml
cargo check --locked 2>/dev/null || cargo check
grep -q "version = \"$next\"" Cargo.lock || {
  echo "lockfile did not pick up $next" >&2; exit 1;
}

git add Cargo.toml Cargo.lock
git commit -m "Release v$next"
git tag "v$next"
git push origin main "v$next"

echo "tag v$next pushed — watch the release:"
echo "  gh run list --workflow release.yml --limit 1"
