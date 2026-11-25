#!/usr/bin/env bash
# Runs tsan-programs.sh on this checkout in Fennec's own Linux VM, the Docker context
# `colima-fennec` (docs/landing.md says how to make it), as CI and the nightly run it on Linux:
#
#   .github/scripts/tsan-in-vm.sh [seconds of fuzzing]
#
# The checkout is copied into the container over a pipe, leaving `target` out, and built there:
# never mounted, as the VM keeps a file of the Mac's open for each one read through a mount
# (fennec#1864). The nightly compiler and the fetched crates are kept in volumes of their own, and
# so is the build, which every checkout shares. So each file copied in is dated when it's copied,
# not when it was last changed: cargo goes by dates, and a build another checkout made after this
# one's last change would otherwise look newer than it, and be run in its place. That cost a
# false failure once, with the threads runtime built from another lane's code; it could as easily
# have been a false pass. Fennec's own crates are built again on every run, about three minutes;
# fetched crates aren't. Each runtime's build stamp, a hash of the source it's from, is printed
# with the commit it was copied from, so a run says what it tested. It never uses Docker's default
# context.
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
# The checkout first, before anything else might read standard input. Without `target`, and without
# `.git`, which in a worktree points back to the Mac: the scripts ask git only where the checkout
# starts, which a fresh repository answers.
mkdir -p /src
# `-m`: dated now, so the shared build never looks newer than this checkout.
tar -x -m -f - -C /src --warning=no-unknown-keyword
# The ThreadSanitizer runtime of GCC, which links the programs, as the image CI runs on has it.
# Debian trixie, not bookworm: the runtime of GCC 12 has no __tsan_memcpy, which the nightly emits.
# `procps` for `ps`, which the fuzzer asks the CPU time of what it runs: without it, it counts time
# on the clock, which a busy VM runs up (fennec#2554).
apt-get update -qq > /dev/null && apt-get install -y -qq git binutils perl libtsan2 procps > /dev/null
rustup component add llvm-tools-preview > /dev/null
rustup toolchain install "$NIGHTLY" --profile minimal --component rust-src > /dev/null
cd /src
git config --global --add safe.directory /src
git init -q .
.github/scripts/tsan-runtime.sh
# What was tested: each runtime by its stamp, which the two have to share.
stamps=""
for library in target/release/libfennec_runtime.a target/release/libfennec_runtime_threads.a; do
  stamp=$(strings "$library" | grep -m 1 -o "fennec-runtime-build:[0-9a-f]*" || true)
  echo "tsan-in-vm: $(basename "$library") is ${stamp:-unstamped}"
  stamps="$stamps ${stamp:-unstamped}"
done
[ "$(echo "$stamps" | tr " " "\n" | sort -u | grep -c .)" = 1 ] || {
  echo "tsan-in-vm: the two runtimes are from different builds" >&2
  exit 1
}
# With addresses not randomized, for this and every program it starts: on Arm, TSan finds no room
# for its shadow memory where the kernel scatters mappings this widely, and stops every program.
setarch "$(uname -m)" -R .github/scripts/tsan-programs.sh target/release/fennec "$SECONDS_OF_FUZZING"
'
# `setarch -R` asks the kernel for a process with addresses not randomized, which Docker's default
# seccomp profile refuses, so this container, in Fennec's own VM, runs without it.
# What the fuzzer finds is kept on the Mac, in the checkout's `target/tsan-found`.
mkdir -p "$root/target/tsan-found"
changed=""
[ -z "$(git -C "$root" status --porcelain --untracked-files=no)" ] || changed=", with changes not committed"
echo "tsan-in-vm: testing $root at $(git -C "$root" rev-parse --short=12 HEAD)$changed"
# macOS's tar leaves out its own metadata, which Linux's tar has no use for.
COPYFILE_DISABLE=1 tar -c --no-xattrs --no-mac-metadata --exclude ./target --exclude ./.git -C "$root" -f - . |
docker --context "$context" run --rm --interactive --memory "$memory" --security-opt seccomp=unconfined \
  --volume "$root/target/tsan-found:/src/found" \
  --volume fennec-tsan-target:/src/target \
  --volume fennec-tsan-rustup:/usr/local/rustup \
  --volume fennec-linux-check-registry:/usr/local/cargo/registry \
  --env NIGHTLY="$nightly" \
  --env RUSTUP_TOOLCHAIN="$toolchain" \
  --env SECONDS_OF_FUZZING="$seconds" \
  "rust:${toolchain%.*}-slim-trixie" \
  bash -c "$inside"
