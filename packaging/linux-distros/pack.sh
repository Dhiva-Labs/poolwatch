#!/usr/bin/env bash
# Repackages one architecture's Linux build for distributions beyond
# Debian/Ubuntu:
#
#   .rpm          Fedora, RHEL/Rocky/Alma, openSUSE
#   .pkg.tar.zst  Arch Linux, Manjaro, EndeavourOS
#   .AppImage     any glibc distribution (GUI apps only, APPIMAGE=1)
#
# usage: pack.sh <root> <amd64|arm64> <outdir>
#
#   <root>    the installed file tree, exactly what the .deb installs, e.g.
#             `dpkg-deb -x foo_1.2.3_amd64.deb root`. Every format is built
#             from this one tree, so all of them ship the same files.
#   <outdir>  where the packages are written.
#
# Package metadata comes from distro.conf next to this script. VERSION must be
# set in the environment (the upstream version, without a Debian revision).
# Needs nfpm and, for the AppImage, appimagetool: on PATH, or set NFPM and
# APPIMAGETOOL. Also objdump (binutils) and file.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ $# -eq 3 ] || { echo "usage: $0 <root> <amd64|arm64> <outdir>" >&2; exit 2; }
src_root="$(cd "$1" && pwd)"
arch="$2"
mkdir -p "$3"
out="$(cd "$3" && pwd)"

# shellcheck source=/dev/null
. "$here/distro.conf"
: "${VERSION:?set VERSION to the package version, e.g. 1.2.3}"
: "${NAME:?distro.conf must set NAME}"
NFPM="${NFPM:-nfpm}"
APPIMAGETOOL="${APPIMAGETOOL:-appimagetool}"
PKG_RELEASE="${PKG_RELEASE:-1}"

case "$arch" in
  amd64) uname_arch=x86_64 ;;
  arm64) uname_arch=aarch64 ;;
  *) echo "unknown arch '$arch' (want amd64 or arm64)" >&2; exit 2 ;;
esac
# Architecture-independent packages (pure Python, shell): nfpm's "all" is
# noarch for rpm and any for Arch.
nfpm_arch="$arch"
if [ "${ARCH_INDEPENDENT:-0}" = 1 ]; then nfpm_arch=all; fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- 1. The tree, minus Debian-only files -----------------------------------
tree="$work/root"
mkdir -p "$tree"
cp -a "$src_root/." "$tree/"
rm -rf "$tree/DEBIAN" "$tree/usr/share/lintian"
if [ -d "$tree/usr/share/doc" ]; then
  find "$tree/usr/share/doc" -name 'changelog.Debian.gz' -delete
fi
if [ -n "${LICENSE_FILE:-}" ]; then
  install -Dm644 "$LICENSE_FILE" "$tree/usr/share/licenses/$NAME/LICENSE"
fi
# Whatever umask built the tree, ship world-readable, root-writable modes.
chmod -R u+rwX,go+rX,go-w "$tree"

# --- 2. Runtime dependencies ------------------------------------------------
# rpm: every shared library the ELF files need that the package does not ship
# itself, as rpm soname dependencies (libgtk-3.so.0()(64bit)). Fedora and
# openSUSE both resolve those, whatever their package is called, so one .rpm
# serves both. The highest GLIBC_ symbol version becomes a dependency too,
# so dnf/zypper refuse a too-old distribution instead of the app crashing.
elfs=()
while IFS= read -r -d '' f; do
  if [ "$(head -c 4 "$f" | od -An -tx1 | tr -d ' \n')" = 7f454c46 ]; then
    elfs+=("$f")
  fi
done < <(find "$tree" -type f -print0)

declare -A provided=() needed=()
glibc_max=
for f in "${elfs[@]}"; do
  provided["$(basename "$f")"]=1
  while read -r tag value; do
    case "$tag" in
      SONAME) provided["$value"]=1 ;;
      NEEDED) needed["$value"]=1 ;;
    esac
  done < <(objdump -p "$f" | awk '$1 == "SONAME" || $1 == "NEEDED" { print $1, $2 }')
  # Static executables (musl, Go) have no dynamic symbol table: objdump -T
  # fails on them, so ask only dynamic ELF files. awk, unlike grep, does not
  # fail the pipeline when a file uses no versioned glibc symbol.
  v=
  if objdump -p "$f" | grep -q '^Dynamic Section:'; then
    v="$(objdump -T "$f" | awk 'match($0, /GLIBC_[0-9][0-9.]*/) { print substr($0, RSTART, RLENGTH) }' | sort -uV | tail -n 1)"
  fi
  if [ -n "$v" ]; then
    glibc_max="$(printf '%s\n%s\n' "$glibc_max" "$v" | sed '/^$/d' | sort -V | tail -n 1)"
  fi
done

rpm_requires=()
for lib in "${!needed[@]}"; do
  [ -n "${provided[$lib]:-}" ] && continue
  rpm_requires+=("$lib()(64bit)")
done
if [ -n "$glibc_max" ]; then rpm_requires+=("libc.so.6($glibc_max)(64bit)"); fi
# Non-library runtime needs (tools the app executes, e.g. /usr/bin/git) come
# from distro.conf. rpm resolves file paths under /usr/bin directly.
rpm_requires+=("${RPM_REQUIRES[@]}")
mapfile -t rpm_requires < <(printf '%s\n' "${rpm_requires[@]}" | sed '/^$/d' | sort -u)

# Arch has no soname dependencies for most libraries, so its list is written
# by hand in distro.conf (Arch package names). CI installs the package on
# archlinux:latest and fails on any "not found" from ldd, which is what keeps
# that list honest.
arch_depends=("${ARCH_DEPENDS[@]}")

# --- 3. nfpm config ---------------------------------------------------------
q() { printf "'%s'" "${1//\'/\'\'}"; }   # YAML single-quoted scalar

# $1: full | short. Arch shows pkgdesc as one line, so it gets the summary
# only; rpm gets the summary plus the long description.
write_config() {
  echo "name: $(q "$NAME")"
  echo "arch: $nfpm_arch"
  echo "platform: linux"
  echo "version: $(q "$VERSION")"
  echo "release: $(q "$PKG_RELEASE")"
  echo "maintainer: $(q "$MAINTAINER")"
  echo "vendor: 'Dhiva Labs'"
  echo "homepage: $(q "$HOMEPAGE")"
  echo "license: $(q "$LICENSE")"
  echo "description: |-"
  if [ "$1" = full ]; then
    printf '%s\n' "$SUMMARY" "" "$DESCRIPTION" | sed 's/^/  /'
  else
    printf '%s\n' "$SUMMARY" | sed 's/^/  /'
  fi
  echo "contents:"
  # Directories: own only the package's private ones (any path with a
  # component named after the package). Owning /usr/bin or
  # /usr/share/applications would collide with the distribution's
  # filesystem package and make dnf refuse the install.
  while IFS= read -r -d '' d; do
    rel="${d#"$tree"}"
    case "/$rel/" in
      */"$NAME"/*) ;;
      *) continue ;;
    esac
    echo "  - dst: $(q "$rel")"
    echo "    type: dir"
    echo "    file_info: { mode: 0$(stat -c %a "$d") }"
  done < <(find "$tree" -mindepth 1 -type d -print0 | sort -z)
  while IFS= read -r -d '' f; do
    rel="${f#"$tree"}"
    if [ -L "$f" ]; then
      echo "  - src: $(q "$(readlink "$f")")"
      echo "    dst: $(q "$rel")"
      echo "    type: symlink"
    else
      echo "  - src: $(q "$f")"
      echo "    dst: $(q "$rel")"
      echo "    file_info: { mode: 0$(stat -c %a "$f") }"
    fi
  done < <(find "$tree" \( -type f -o -type l \) -print0 | sort -z)
  echo "overrides:"
  echo "  rpm:"
  echo "    depends:"
  for d in "${rpm_requires[@]}"; do echo "      - $(q "$d")"; done
  echo "  archlinux:"
  if [ "${#arch_depends[@]}" -gt 0 ]; then
    echo "    depends:"
    for d in "${arch_depends[@]}"; do echo "      - $(q "$d")"; done
  else
    echo "    depends: []"
  fi
  echo "rpm:"
  echo "  summary: $(q "$SUMMARY")"
  echo "  compression: zstd"
  [ -n "${RPM_GROUP:-}" ] && echo "  group: $(q "$RPM_GROUP")"
  echo "archlinux:"
  echo "  packager: $(q "$MAINTAINER")"
  if [ -n "${POSTINSTALL:-}" ] || [ -n "${POSTREMOVE:-}" ]; then
    echo "scripts:"
    [ -n "${POSTINSTALL:-}" ] && echo "  postinstall: $(q "$here/$POSTINSTALL")"
    [ -n "${POSTREMOVE:-}" ] && echo "  postremove: $(q "$here/$POSTREMOVE")"
  fi
  true
}

write_config full > "$work/nfpm-rpm.yaml"
write_config short > "$work/nfpm-arch.yaml"
if [ "${PACK_DEBUG:-0}" = 1 ]; then cat "$work/nfpm-rpm.yaml"; fi
"$NFPM" pkg --config "$work/nfpm-rpm.yaml" --packager rpm --target "$out/"
"$NFPM" pkg --config "$work/nfpm-arch.yaml" --packager archlinux --target "$out/"

# --- 4. AppImage ------------------------------------------------------------
# The AppImage carries the same tree. Libraries every desktop already has
# (GTK 3, glib, glibc, Mesa) stay on the host, as with every other GTK
# AppImage; anything rarer must already be inside the tree (bundled by the
# app's own build), or the AppImage fails on hosts without it.
if [ "${APPIMAGE:-0}" = 1 ]; then
  : "${APP_NAME:?distro.conf must set APP_NAME for the AppImage}"
  : "${EXEC:?distro.conf must set EXEC (main executable, relative to the root)}"
  : "${DESKTOP:?distro.conf must set DESKTOP (.desktop file, relative to the root)}"
  : "${ICON:?distro.conf must set ICON (PNG icon, relative to the root)}"
  appdir="$work/AppDir"
  mkdir -p "$appdir"
  cp -a "$tree/." "$appdir/"
  [ -x "$appdir/$EXEC" ] || { echo "EXEC $EXEC is not an executable in the tree" >&2; exit 1; }

  desktop="$appdir/$(basename "$DESKTOP")"
  cp "$tree/$DESKTOP" "$desktop"
  icon="$(sed -n 's/^Icon=//p' "$desktop" | head -n 1)"
  case "$icon" in
    /*|*.png|*.svg) icon="$(basename "${icon%.*}")"; sed -i "s|^Icon=.*|Icon=$icon|" "$desktop" ;;
  esac
  [ -n "$icon" ] || { echo "$DESKTOP has no Icon= line" >&2; exit 1; }
  cp "$tree/$ICON" "$appdir/$icon.png"
  ln -sf "$icon.png" "$appdir/.DirIcon"

  {
    echo '#!/bin/sh'
    echo 'here="$(dirname "$(readlink -f "$0")")"'
    echo 'export XDG_DATA_DIRS="$here/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"'
    # Extra environment the .deb's /usr/bin launcher sets, if any.
    if [ -n "${APPRUN_ENV:-}" ]; then printf '%s\n' "$APPRUN_ENV"; fi
    echo "exec \"\$here/$EXEC\" \"\$@\""
  } > "$appdir/AppRun"
  chmod 0755 "$appdir/AppRun"

  # No FUSE on CI runners or in containers.
  export APPIMAGE_EXTRACT_AND_RUN=1
  ARCH="$uname_arch" "$APPIMAGETOOL" --no-appstream "$appdir" \
    "$out/$APP_NAME-$VERSION-$uname_arch.AppImage"
fi

ls -l "$out"
