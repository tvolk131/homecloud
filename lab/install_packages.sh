#!/bin/sh
set -eu
test "$(id -u)" -eq 0
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y incus qemu-system-x86 qemu-utils ovmf \
  qemu-system-modules-spice genisoimage zfsutils-linux smartmontools
