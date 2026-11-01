#!/usr/bin/env bash
# Installs what pack.sh built on the real distributions and runs it there.
#
# usage: verify.sh <outdir> <amd64|arm64>
#
# Uses docker images of the runner's own architecture, so the arm64 leg
# tests aarch64 packages natively:
#
#   fedora:latest          the .rpm (dnf resolves its soname dependencies)
#   opensuse/tumbleweed    the same .rpm (zypper, different package names)
#   archlinux:latest       the .pkg.tar.zst (x86_64 only: Arch has no
#                          official arm64 image; arm64 gets a static check)
#   fedora:latest          the AppImage, on a host that is not Debian-based
#
# In each container: install, ldd every ELF file the package installed (any
# "not found" fails the job), then the smoke test from distro.conf. The
# checkout is mounted read-only at /repo for smoke tests that use the repo's
# own scripts and fixtures.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
[ $# -eq 2 ] || { echo "usage: $0 <outdir> <amd64|arm64>" >&2; exit 2; }
out="$(cd "$1" && pwd)"
arch="$2"
# shellcheck source=/dev/null
. "$here/distro.conf"
case "$arch" in
  amd64) uname_arch=x86_64; elf_machine='x86-64' ;;
  arm64) uname_arch=aarch64; elf_machine='ARM aarch64' ;;
  *) echo "unknown arch '$arch'" >&2; exit 2 ;;
esac
[ "$(uname -m)" = "$uname_arch" ] || {
  echo "verify.sh must run on a $uname_arch runner (this is $(uname -m))" >&2
  exit 1
}

shopt -s nullglob
rpms=("$out"/*."$uname_arch".rpm "$out"/*.noarch.rpm)
pkgs=("$out"/*-"$uname_arch".pkg.tar.zst "$out"/*-any.pkg.tar.zst)
appimages=("$out"/*-"$uname_arch".AppImage)
shopt -u nullglob
[ "${#rpms[@]}" -eq 1 ] || { echo "expected one .rpm in $out, found ${#rpms[@]}" >&2; exit 1; }
[ "${#pkgs[@]}" -eq 1 ] || { echo "expected one .pkg.tar.zst in $out, found ${#pkgs[@]}" >&2; exit 1; }

run_in() {
  local image="$1" mode="$2" file="$3"
  echo "::group::$mode on $image: $(basename "$file")"
  docker run --rm \
    -v "$out:/pkgs:ro" -v "$here:/linux-distros:ro" -v "$repo:/repo:ro" \
    -e MODE="$mode" -e FILE="/pkgs/$(basename "$file")" \
    "$image" bash /linux-distros/in-container.sh
  echo "::endgroup::"
}

run_in fedora:latest rpm "${rpms[0]}"
run_in opensuse/tumbleweed:latest rpm-suse "${rpms[0]}"

if [ "$uname_arch" = x86_64 ]; then
  run_in archlinux:latest pacman "${pkgs[0]}"
else
  # No official Arch Linux ARM image to install into. Check what pacman
  # would read: the declared architecture, and that the ELF files inside
  # really are aarch64.
  echo "::group::static check of $(basename "${pkgs[0]}")"
  tmp="$(mktemp -d)"
  zstd -dc "${pkgs[0]}" | tar -x -C "$tmp"
  grep -qx "arch = aarch64" "$tmp/.PKGINFO" || { cat "$tmp/.PKGINFO"; exit 1; }
  if [ "${ARCH_INDEPENDENT:-0}" != 1 ]; then
    n=0
    while IFS= read -r -d '' f; do
      if [ "$(head -c 4 "$f" | od -An -tx1 | tr -d ' \n')" = 7f454c46 ]; then
        file -b "$f" | grep -qF "$elf_machine" || { echo "not $elf_machine: $f"; file "$f"; exit 1; }
        n=$((n + 1))
      fi
    done < <(find "$tmp" -type f -print0)
    [ "$n" -gt 0 ] || { echo "no ELF files in the package" >&2; exit 1; }
    echo "$n ELF files, all $elf_machine"
  fi
  rm -rf "$tmp"
  echo "::endgroup::"
fi

if [ "${APPIMAGE:-0}" = 1 ]; then
  [ "${#appimages[@]}" -eq 1 ] || { echo "expected one AppImage, found ${#appimages[@]}" >&2; exit 1; }
  file -b "${appimages[0]}" | grep -qF "$elf_machine"
  run_in fedora:latest appimage "${appimages[0]}"
fi
echo "all distribution checks passed for $arch"
