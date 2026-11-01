#!/usr/bin/env bash
# Renders the AUR PKGBUILD for one release and proves it builds.
#
# usage: aur.sh <x86_64.tar.gz> <aarch64.tar.gz> <outdir>
#
# Fills PKGBUILD.in with the version, the release download URL and the
# sha256 of both release archives, then, in an archlinux:latest container,
# generates .SRCINFO, runs makepkg on the x86_64 source (the archive is
# placed next to the PKGBUILD under its release file name, so makepkg
# verifies the checksum without downloading), installs the result and runs
# it. Writes <name>-bin-<version>-aur.tar.gz holding PKGBUILD and .SRCINFO,
# ready to push to the AUR.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ $# -eq 3 ] || { echo "usage: $0 <x86_64.tar.gz> <aarch64.tar.gz> <outdir>" >&2; exit 2; }
tar_x86_64="$(readlink -f "$1")"; tar_aarch64="$(readlink -f "$2")"
mkdir -p "$3"; out="$(cd "$3" && pwd)"
# shellcheck source=/dev/null
. "$here/../linux-distros/distro.conf"
: "${VERSION:?set VERSION}"
: "${GITHUB_REPO:?distro.conf must set GITHUB_REPO}"
tag="${TAG_PREFIX-v}$VERSION"
# The names release.yml gives the archives on a tag build. Pull-request dry
# runs name theirs test-<sha>, so the files are renamed, not looked up.
name_x86_64="$NAME-$tag-x86_64-linux-musl.tar.gz"
name_aarch64="$NAME-$tag-aarch64-linux-musl.tar.gz"

work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
sed -e "s|@NAME@|$NAME|g" -e "s|@VERSION@|$VERSION|g" \
    -e "s|@SUMMARY@|${SUMMARY//\'/}|g" -e "s|@HOMEPAGE@|$HOMEPAGE|g" \
    -e "s|@LICENSE@|$LICENSE|g" \
    -e "s|@RELEASE_URL@|https://github.com/$GITHUB_REPO/releases/download/$tag|g" \
    -e "s|@TARBALL_X86_64@|$name_x86_64|g" -e "s|@TARBALL_AARCH64@|$name_aarch64|g" \
    -e "s|@SHA_X86_64@|$(sha256sum "$tar_x86_64" | cut -d' ' -f1)|g" \
    -e "s|@SHA_AARCH64@|$(sha256sum "$tar_aarch64" | cut -d' ' -f1)|g" \
    "$here/PKGBUILD.in" > "$work/PKGBUILD"
cp "$tar_x86_64" "$work/$name_x86_64"
if grep -n '@[A-Z0-9_]*@' "$work/PKGBUILD"; then echo "unfilled placeholder" >&2; exit 1; fi

# makepkg refuses to run as root. The build user gets the runner's uid, so
# everything it writes into the bind mount stays removable by the trap.
docker run --rm -v "$work:/build" -e HOST_UID="$(id -u)" -e NAME="$NAME" \
  archlinux:latest bash -euo pipefail -c '
  pacman -Syu --noconfirm --needed -q base-devel namcap
  useradd -m -u "$HOST_UID" builder
  chown -R builder /build
  cd /build
  su builder -c "makepkg --printsrcinfo > .SRCINFO"
  su builder -c "makepkg --noconfirm --nodeps -f"
  namcap PKGBUILD
  pacman -U --noconfirm ./*-x86_64.pkg.tar.zst
  pacman -Qlq "$NAME-bin"
  "$NAME" --version
  cat .SRCINFO
'
tar -C "$work" -czf "$out/$NAME-bin-$VERSION-aur.tar.gz" PKGBUILD .SRCINFO
ls -l "$out/$NAME-bin-$VERSION-aur.tar.gz"
