#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
# Fixed software renderer + bundled Roboto; no display server or host access.
export ICED_TEST_BACKEND=tiny-skia
cargo test --locked -p homecloud-desktop -- --test-threads=1
