#!/bin/sh
# Run as the guest's dev user, never as root. Provider login is owner-managed.
set -eu
test "$(id -u)" -ne 0 || { printf '%s\n' 'Run this as dev, not root.' >&2; exit 1; }
version="${1:?Pass the exact version matching your T3 desktop client}"
case "$version" in *[!0-9A-Za-z.+-]*|'') exit 2 ;; esac
umask 077
mkdir -p "$HOME/.t3" "$HOME/.local/bin"
installer="$(mktemp)"
trap 'rm -f "$installer"' EXIT
curl --proto '=https' --tlsv1.2 -fsSL https://t3.codes/install.sh -o "$installer"
T3CODE_VERSION="$version" sh "$installer"
# SSH forwards reach loopback; no guest T3 port is exposed to other VMs or LAN.
envfile="$HOME/.t3/service.env"
if [ -e "$envfile" ]; then
  printf '%s\n' 'Existing service.env retained; verify T3CODE_HOST=127.0.0.1 before proceeding.' >&2
  grep -qx 'T3CODE_HOST=127.0.0.1' "$envfile" || exit 1
else
  printf 'T3CODE_HOST=127.0.0.1\nT3CODE_PORT=3773\n' > "$envfile"
fi
uid="$(id -u)"
export PATH="$HOME/.local/bin:$PATH"
export XDG_RUNTIME_DIR="/run/user/$uid"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
t3 service install
