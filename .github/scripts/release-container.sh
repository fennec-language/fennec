#!/usr/bin/env bash
# Installs what a release is built with in its container, at the versions it's built with, and
# rustup, which installs the toolchain `rust-toolchain.toml` names. CI runs it in the release's
# containers, and so does a rebuild before signing (`rebuild-release.sh`), so both build with the
# same compilers:
#
#   .github/scripts/release-container.sh ubuntu    the musl runtime's, on ubuntu:24.04
#   .github/scripts/release-container.sh alma      the archives', on almalinux:8
#
# Each is pinned, as each image is by its digest. Ubuntu's are every package installed, each by its
# SHA-256, in `.github/release-debs.lock`, fetched from Launchpad, which keeps every version, where
# Ubuntu's archive drops one once a newer one is out. A file that's missing or isn't the one pinned
# stops it, and so does anything the lock leaves out. `release-debs.sh` writes the lock, and moves
# the pins on. AlmaLinux's are moved on by hand, in the same PR as anything they change, when a
# version it no longer has fails the install. Git and what fetches rustup aren't pinned on
# AlmaLinux: nothing they do goes in what's built.
set -euo pipefail
case "${1:?say which: ubuntu or alma}" in
  ubuntu)
    export DEBIAN_FRONTEND=noninteractive
    # musl's `musl-gcc` compiles the runtime's C parts with GCC, and GNU's assembler assembles
    # ring's; musl's own C library comes from Rust's toolchain. `gcc` is the package that names
    # GCC 13 `gcc` and `cc`, which `musl-gcc` and Rust's build scripts run.
    lock=.github/release-debs.lock
    image=$(sed -n 's/^image //p' "$lock")
    if [ -z "$image" ] || ! grep -qF "\"$image\"" .github/workflows/release.yml; then
      echo "release-container: $lock was written for ${image:-no image}, not the one release.yml pins; run .github/scripts/release-debs.sh" >&2
      exit 1
    fi
    arch=$(dpkg --print-architecture)
    debs=$(mktemp -d)
    # apt's own downloader, since the image has no other, checks each file's hash as it goes.
    while read -r _ name version sha url; do
      if ! /usr/lib/apt/apt-helper -o Acquire::Retries=3 -o APT::Sandbox::User=root download-file "$url" "$debs/${url##*/}" "SHA256:$sha"; then
        echo "release-container: $name $version for $arch, $url, couldn't be fetched, or isn't the file $lock pins" >&2
        exit 1
      fi
    done < <(grep "^$arch " "$lock")
    if [ -z "$(ls "$debs")" ]; then
      echo "release-container: $lock pins nothing for $arch" >&2
      exit 1
    fi
    # Only those files, all at once, by dpkg: it gets nothing from anywhere else, so anything the lock
    # leaves out fails the install.
    dpkg --install --force-confold "$debs"/*.deb > /dev/null
    /bin/rm -rf "$debs"
    while read -r _ name version _; do
      # The format is dpkg-query's, not the shell's.
      # shellcheck disable=SC2016
      installed=$(dpkg-query -W -f '${Version}' "$name" 2> /dev/null || true)
      if [ "$installed" != "$version" ]; then
        echo "release-container: $name is ${installed:-not installed}, not the $version $lock pins" >&2
        exit 1
      fi
    done < <(grep "^$arch " "$lock")
    ;;
  alma)
    # A C compiler for mimalloc and ring, GNU's assembler, and what packing takes.
    dnf install -y -q gcc-8.5.0-31.el8_10.alma.1 gcc-c++-8.5.0-31.el8_10.alma.1 binutils-2.30-128.el8_10 \
      make git tar gzip findutils diffutils perl
    ;;
  *)
    echo "release-container: say ubuntu or alma, not $1" >&2
    exit 2
    ;;
esac
curl -sSfL --retry 3 https://sh.rustup.rs | sh -s -- -y --default-toolchain none --profile minimal --no-modify-path
