#!/usr/bin/env bash
# The workspace and its tests compile on Linux with every warning an error, as CI builds them, so
# code that's only right on macOS, like a helper only a macOS test calls, fails here rather than
# on main. It's `cargo check` in a Linux container: a check for another target from a Mac can't
# build the C that ring, SQLite and mimalloc carry. Windows isn't checked: that needs Microsoft's
# C toolchain, which a Mac doesn't have.
#
# The container's target folder is a Docker volume of this worktree's own, since worktrees that
# share one build stale code; crates fetched are kept in one volume they share.
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
volume="fennec-linux-check-$(printf '%s' "$root" | shasum -a 256 | cut -c1-16)"
# The image's own Rust, rather than the exact patch release the toolchain file names, which the
# container would otherwise download every time.
# shellcheck disable=SC2016
docker run --rm --memory 4g \
  --volume "$root:/src:ro" \
  --volume "$volume:/target" \
  --volume fennec-linux-check-registry:/usr/local/cargo/registry \
  --env CARGO_TARGET_DIR=/target \
  --env RUSTFLAGS="-D warnings" \
  --workdir /src \
  "$image" \
  sh -c 'RUSTUP_TOOLCHAIN="$(rustup default | cut -d" " -f1)" exec cargo check --locked --workspace --tests'
