# lilbox-box — a base box image with Tailscale baked in.
#
# Built with Docker, imported into microsandbox (see build.sh). Booting this
# image as a microVM gives a box that already has `tailscale` + `tailscaled`
# and a first-boot bring-up hook, so it can join your tailnet as its own node.
#
# Auth (joining the tailnet as a tagged ephemeral node) is layered on top by
# `lilbox new` — it injects an ephemeral, tag:lilbox-vm auth key and runs
# `tailscale up`. This image only bakes in the binaries + the bring-up hook.
FROM alpine:3.24

# tailscale ships both `tailscale` and `tailscaled` in one apk package.
# iproute2 gives a real `ip` (busybox's lacks `tuntap`); ca-certificates for
# TLS to the control plane; openssh for Tailscale SSH later (#6); socat for
# the unix→vsock herdr socket relay (#130).
RUN apk add --no-cache \
      tailscale \
      iproute2 \
      ca-certificates \
      openssh \
      socat

# State + runtime dirs tailscaled expects.
RUN mkdir -p /var/lib/tailscale /var/run/tailscale

# First-boot bring-up hook (starts tailscaled on a kernel tun).
COPY lilbox-boot /usr/local/bin/lilbox-boot
RUN chmod +x /usr/local/bin/lilbox-boot

LABEL dev.lilbox.image="lilbox-box" \
      dev.lilbox.tailscale="baked"
