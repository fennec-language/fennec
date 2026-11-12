#!/usr/bin/env bash
# The workspace and its tests compile on Linux with every warning an error, as CI builds them, so
# code that's only right on macOS, like a helper only a macOS test calls, fails here rather than
# on main. It's `cargo check` in a Linux container: a check for another target from a Mac can't
# build the C that ring, SQLite and mimalloc carry. Windows is `windows-check.sh`.
#
# It runs only in Fennec's own Linux VM, the Docker context `colima-fennec` (`colima start fennec`,
# or another name in FENNEC_DOCKER_CONTEXT), never in whatever Docker's default is: on this Mac
# that's another project's VM, which Fennec doesn't use. Where Fennec's VM isn't there, isn't
# running, or its disk is full, the step says the Linux check is unavailable and the gate goes
# on: CI's Linux jobs still check.
#
# In it:
# - One check runs at a time, whichever worktree's gate starts it: two building SQLite at once had
#   the compiler killed for memory, failing gates that had nothing wrong.
# - Each is held to what the VM holds, so a build too big is stopped in its own container.
# - Every worktree shares one target volume and one of fetched crates, rather than a volume each,
#   which filled the VM's disk. Taking turns, they never build in it at once, and this kit's own
#   crates are cleaned out first, so one worktree never takes another's build of them as fresh:
#   every checkout is mounted at the same path, and cargo goes by the times files changed.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
context=${FENNEC_DOCKER_CONTEXT:-colima-fennec}
unavailable() {
  echo "warning: the Linux check is unavailable: $*; the gate goes on without it, and CI's Linux jobs still check" >&2
  exit 0
}
command -v docker > /dev/null || unavailable "there's no docker"
docker context inspect "$context" > /dev/null 2>&1 \
  || unavailable "there's no Docker context \`$context\`, Fennec's own Linux VM"
docker --context "$context" info > /dev/null 2>&1 \
  || unavailable "Fennec's Linux VM, \`$context\`, isn't running"
toolchain=$(sed -n 's/^channel = "\(.*\)"$/\1/p' "$root/rust-toolchain.toml")
# The image of that release; its patch release may be newer, which builds the same.
image="rust:${toolchain%.*}-slim-bookworm"
# What the VM holds, less an eighth for itself, in MiB.
memory=$(docker --context "$context" info --format '{{.MemTotal}}')
memory=$((memory * 7 / 8 / 1024 / 1024))m
# Held by `flock` on a file of this user's, kept open across the `exec`, until the container is done.
lock="${TMPDIR:-/tmp}/fennec-linux-check.lock"
# Inside: the image's own Rust, rather than the exact patch release the toolchain file names,
# which would be downloaded every time; this kit's crates cleaned out; then the check.
# shellcheck disable=SC2016
inside='
export RUSTUP_TOOLCHAIN="$(rustup default | cut -d" " -f1)"
set --
for manifest in crates/*/Cargo.toml; do
  set -- "$@" -p "$(sed -n "s/^name = \"\(.*\)\"$/\1/p" "$manifest" | head -n 1)"
done
cargo clean --locked --quiet "$@" && exec cargo check --locked --workspace --tests
'
said=$(mktemp)
trap 'rm -f "$said"' EXIT
status=0
# shellcheck disable=SC2016
perl -MFcntl=:flock -e '$^F = 255; open(my $held, ">>", shift) or die "linux-check: $!\n"; flock($held, LOCK_EX) or die "linux-check: $!\n"; exec @ARGV or die "linux-check: $!\n"' "$lock" \
  docker --context "$context" run --rm --memory "$memory" \
  --volume "$root:/src:ro" \
  --volume fennec-linux-check-target:/target \
  --volume fennec-linux-check-registry:/usr/local/cargo/registry \
  --env CARGO_TARGET_DIR=/target \
  --env RUSTFLAGS="-D warnings" \
  --workdir /src \
  "$image" \
  sh -c "$inside" 2>&1 | tee "$said" || status=$?
# A full disk is the VM's, not the code's.
if [ "$status" -ne 0 ] && grep -q "No space left on device" "$said"; then
  unavailable "Fennec's Linux VM, \`$context\`, has no disk left"
fi
exit "$status"
