#!/usr/bin/env bash
# Installs what a release is built with in its container, at the versions it's built with, and
# rustup, which installs the toolchain `rust-toolchain.toml` names. CI runs it in the release's
# containers, and so does a rebuild before signing (`rebuild-release.sh`), so both build with the
# same compilers:
#
#   .github/scripts/release-container.sh ubuntu    the musl runtime's, on ubuntu:24.04
#   .github/scripts/release-container.sh alma      the archives', on almalinux:8
#
# Each is pinned, as each image is by its digest. A version the distribution no longer has fails
# the install, and the pin is moved on by hand, in the same PR as anything it changes. Git and
# what fetches rustup aren't pinned: nothing they do goes in what's built.
set -euo pipefail
case "${1:?say which: ubuntu or alma}" in
  ubuntu)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    # musl's `musl-gcc` compiles the runtime's C parts with GCC, and GNU's assembler assembles
    # ring's; musl's own C library comes from Rust's toolchain.
    # `gcc` is the package that names GCC 13 `gcc` and `cc`, which `musl-gcc` and Rust's build
    # scripts run.
    apt-get install -y -qq --no-install-recommends ca-certificates curl git \
      musl-tools=1.2.4-2 gcc=4:13.2.0-7ubuntu1 gcc-13=13.3.0-6ubuntu2~24.04.1 cpp-13=13.3.0-6ubuntu2~24.04.1 \
      binutils=2.42-4ubuntu2.11 libc6-dev=2.39-0ubuntu8.12
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
