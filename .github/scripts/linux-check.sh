#!/usr/bin/env bash
# The workspace and its tests compile on Linux with every warning an error, as CI builds them, so
# code that's only right on macOS, like a helper only a macOS test calls, fails here rather than
# on main. It's `cargo check` in a Linux container: a check for another target from a Mac can't
# build the C that ring, SQLite and mimalloc carry. Windows is `windows-check.sh`.
#
# Docker's Linux VM on a Mac can be small, 2 GB of memory and a few GB of disk, and shared with
# other projects. So:
# - One check runs at a time, whichever worktree's gate starts it: two building SQLite at once had
#   the compiler killed for memory, failing gates that had nothing wrong.
# - Each is held to what the VM holds, so a build too big is stopped in its own container.
# - Every worktree shares one target volume and one of fetched crates, rather than a volume each,
#   which filled the VM's disk. Taking turns, they never build in it at once, and this kit's own
#   crates are cleaned out first, so one worktree never takes another's build of them as fresh:
#   every checkout is mounted at the same path, and cargo goes by the times files changed.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
command -v docker > /dev/null || {
  echo "linux-check: there's no docker to check on Linux with: install one, like colima, and start it" >&2
  exit 1
}
docker info > /dev/null 2>&1 || {
  echo "linux-check: docker isn't reachable: start its Linux VM, like \`colima start\`" >&2
  exit 1
}
toolchain=$(sed -n 's/^channel = "\(.*\)"$/\1/p' "$root/rust-toolchain.toml")
# The image of that release; its patch release may be newer, which builds the same.
image="rust:${toolchain%.*}-slim-bookworm"
# What the VM holds, less an eighth for itself, in MiB.
memory=$(docker info --format '{{.MemTotal}}')
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
perl -MFcntl=:flock -e '$^F = 255; open(my $held, ">>", shift) or die "linux-check: $!\n"; flock($held, LOCK_EX) or die "linux-check: $!\n"; exec @ARGV or die "linux-check: $!\n"' "$lock" \
  docker run --rm --memory "$memory" \
  --volume "$root:/src:ro" \
  --volume fennec-linux-check-target:/target \
  --volume fennec-linux-check-registry:/usr/local/cargo/registry \
  --env CARGO_TARGET_DIR=/target \
  --env RUSTFLAGS="-D warnings" \
  --workdir /src \
  "$image" \
  sh -c "$inside"
