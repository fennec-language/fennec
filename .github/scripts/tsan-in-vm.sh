#!/usr/bin/env bash
# Runs tsan-programs.sh on this checkout in Fennec's own Linux VM, the Docker context
# `colima-fennec` (docs/landing.md says how to make it), as CI and the nightly run it on Linux:
#
#   .github/scripts/tsan-in-vm.sh [seconds of fuzzing]
#
# The checkout is copied into the container, leaving `target` out, and built there. The nightly
# compiler, fetched crates and the build are kept in volumes of their own, so a second run builds
# only what changed. It never uses Docker's default context.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
seconds=${1:-300}
context=${FENNEC_DOCKER_CONTEXT:-colima-fennec}
docker --context "$context" info > /dev/null 2>&1 || {
  colima start fennec --activate=false > /dev/null 2>&1 || {
    echo "tsan-in-vm: Fennec's Linux VM, \`$context\`, isn't there: docs/landing.md says how to make it" >&2
    exit 1
  }
}
toolchain=$(sed -n 's/^channel = "\(.*\)"$/\1/p' "$root/rust-toolchain.toml")
nightly=$(sed -n 's/^ *NIGHTLY: \(nightly-[0-9-]*\)$/\1/p' "$root/.github/workflows/ci.yml" | head -n 1)
memory=$(docker --context "$context" info --format '{{.MemTotal}}')
memory=$((memory * 7 / 8 / 1024 / 1024))m
# shellcheck disable=SC2016
inside='
set -euo pipefail
# The ThreadSanitizer runtime of GCC, which links the programs, as the image CI runs on has it.
# Debian trixie, not bookworm: the runtime of GCC 12 has no __tsan_memcpy, which the nightly emits.
apt-get update -qq > /dev/null && apt-get install -y -qq git binutils perl libtsan2 > /dev/null
rustup component add llvm-tools-preview > /dev/null
rustup toolchain install "$NIGHTLY" --profile minimal --component rust-src > /dev/null
mkdir -p /src
# Without `target`, and without `.git`, which in a worktree points back to the Mac: the scripts ask
# git only where the checkout starts, which a fresh repository answers.
cd /repo && tar --exclude=./target --exclude=./.git -cf - . | (cd /src && tar -xf -)
cd /src
git config --global --add safe.directory /src
git init -q .
.github/scripts/tsan-runtime.sh
.github/scripts/tsan-programs.sh target/release/fennec "$SECONDS_OF_FUZZING"
'
docker --context "$context" run --rm --memory "$memory" \
  --volume "$root:/repo:ro" \
  --volume fennec-tsan-target:/src/target \
  --volume fennec-tsan-rustup:/usr/local/rustup \
  --volume fennec-linux-check-registry:/usr/local/cargo/registry \
  --env NIGHTLY="$nightly" \
  --env RUSTUP_TOOLCHAIN="$toolchain" \
  --env SECONDS_OF_FUZZING="$seconds" \
  "rust:${toolchain%.*}-slim-trixie" \
  bash -c "$inside"
