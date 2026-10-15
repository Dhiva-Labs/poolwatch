#!/usr/bin/env bash
# Asserts a Linux release binary is the expected CPU architecture and fully
# static (no dynamic loader, no shared libraries).
#
# usage: verify-linux-binary.sh <binary> <architecture as printed by `file`>
#   e.g. verify-linux-binary.sh poolwatch "ARM aarch64"
set -euo pipefail

bin=$1
arch=$2

description=$(file "$bin")
echo "$description"

if ! grep -qF "$arch" <<<"$description"; then
  echo "::error::$bin is not '$arch'"
  exit 1
fi
if ! grep -qE "statically linked|static-pie linked" <<<"$description"; then
  echo "::error::$bin is not statically linked according to file(1)"
  exit 1
fi

# ldd wording differs by binary flavor: "not a dynamic executable" (static)
# vs "statically linked" (static-pie).
ldd_out=$(ldd "$bin" 2>&1 || true)
echo "ldd: $ldd_out"
if ! grep -qE "statically linked|not a dynamic executable" <<<"$ldd_out"; then
  echo "::error::$bin is not static (ldd lists shared libraries)"
  exit 1
fi
