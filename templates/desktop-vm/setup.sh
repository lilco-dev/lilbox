#!/bin/sh
# desktop-vm provisioning: the wrapvm base image (ghcr.io/tobi/wrapvm:desktop)
# carries its whole desktop stack behind its CMD (`wrap-desktop run`), but a
# microsandbox guest never executes the image CMD — lilbox boots detached and
# provisions over exec. So this script's one job is to launch that stack and
# leave it running. Idempotent — re-runs are no-ops while the desktop is up.
set -eu

WRAP_DESKTOP=/opt/wrap/bin/wrap-desktop

if [ ! -x "$WRAP_DESKTOP" ]; then
  echo "desktop-vm: $WRAP_DESKTOP not found — is this the wrapvm image?" >&2
  exit 1
fi

if pgrep -f "wrap-desktop run" >/dev/null 2>&1; then
  echo "desktop-vm: wrap-desktop already running, nothing to do"
  exit 0
fi

# tailscale serve on the host preserves the browser's Host header, and voxl
# refuses names it wasn't started with (DNS-rebinding guard). Scoping to the
# .ts.net suffix covers `<host>.<tailnet>.ts.net` however the box is exposed,
# without opening the door to arbitrary names.
export WRAP_DESKTOP_VOXL_ARGS="${WRAP_DESKTOP_VOXL_ARGS:-} --allowed-hosts=.ts.net"

# setsid + nohup so the Xvfb/XFCE/Chrome/voxl tree survives the provisioning
# session that spawned it; logs land in /var/log for `lilbox logs`-style
# debugging via `lilbox ssh <box> -- tail -f /var/log/wrap-desktop.log`.
setsid nohup "$WRAP_DESKTOP" run >/var/log/wrap-desktop.log 2>&1 &

# Wait for the voxl viewer to accept connections before declaring success —
# `lilbox expose` right after `new` should hit a live :6080. Any HTTP response
# counts: voxl answers 401 until you sign in, so `curl -f` would never pass.
n=0
while [ "$n" -lt 60 ]; do
  if curl -s -o /dev/null http://127.0.0.1:6080/ 2>/dev/null; then
    echo "desktop-vm ready: voxl viewer on :6080 (expose with 'lilbox expose <box>')"
    exit 0
  fi
  n=$((n + 1))
  sleep 2
done

echo "desktop-vm: viewer on :6080 did not come up within 120s — check /var/log/wrap-desktop.log" >&2
exit 1
