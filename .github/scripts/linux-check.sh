#!/usr/bin/env bash
# The workspace and its tests compile on Linux with every warning an error, as CI builds them, so
# code that's only right on macOS, like a helper only a macOS test calls, fails here rather than
# on main. It's `cargo check` in a Linux container: a check for another target from a Mac can't
# build the C that ring, SQLite and mimalloc carry. Windows is `windows-check.sh`.
#
# It runs only in Fennec's own Linux VM, the colima profile `fennec` and its Docker context
# `colima-fennec` (or another context in FENNEC_DOCKER_CONTEXT), never in whatever Docker's
# default is: on this Mac that's another project's VM, which Fennec doesn't use. docs/landing.md
# says how to make it. Where it isn't there, won't start, or its disk is full, the step says the
# Linux check is unavailable and the gate goes on: CI's Linux jobs still check.
#
# - The VM is started when a check needs it, without making it Docker's context, and stopped
#   after 30 minutes with no check and no container running, so its memory isn't held around the
#   clock. Starting it is part of the step's time.
# - One check runs at a time, whichever worktree's gate starts it, under a lock this script takes
#   and runs itself again under: two building SQLite at once had the compiler killed for memory.
# - Each is held to what the VM holds, so a build too big is stopped in its own container.
# - Every worktree shares one target volume and one of fetched crates. This kit's own crates are
#   cleaned out before each check, so one worktree never takes another's build of them as fresh:
#   every checkout is at the same path, and cargo goes by the times files changed.
# - The checkout goes in as a copy, over a pipe, into a volume of the VM's own, never mounted: the
#   VM keeps a file of the Mac's open for each one it has read through a mount, for as long as it
#   runs, and one running all day held 140,000, every source file of 105 worktrees, 78 of them
#   removed, near half of what the whole Mac may have open (fennec#1864).
# - When the VM's disk is past 75% full, its `fennec-` volumes are removed first, and built again.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
context=${FENNEC_DOCKER_CONTEXT:-colima-fennec}
lock="${TMPDIR:-/tmp}/fennec-linux-check.lock"
used="${TMPDIR:-/tmp}/fennec-linux-check.used"
unavailable() {
  echo "warning: the Linux check is unavailable: $*; the gate goes on without it, and CI's Linux jobs still check" >&2
  exit 0
}
# The lock first, held by `flock` on a file of this user's, kept open across the `exec`, for as
# long as this script, run again under it, takes.
if [ -z "${FENNEC_LINUX_CHECK_HELD:-}" ]; then
  # The lock's descriptor goes in FENNEC_LINUX_CHECK_HELD, so what outlasts the check can close it.
  exec perl -MFcntl=:flock -e '$^F = 255; open(my $held, ">>", shift) or die "linux-check: $!\n"; flock($held, LOCK_EX) or die "linux-check: $!\n"; $ENV{FENNEC_LINUX_CHECK_HELD} = fileno($held); exec @ARGV or die "linux-check: $!\n"' \
    "$lock" "$0" "$@"
fi
# However it ends, the VM is stopped once 30 minutes pass with no check after this one: a stopper
# of its own sleeps that long, and stops it only if no check has run since, none holds the lock,
# and no container is running in it, as another lane's profiling or TSan build may be, which a
# stop killed on 2026-11-16. Run through `eval` by the stopper below.
# shellcheck disable=SC2016,SC2034
stop_when_idle='
sleep "$(($3 * 60))"
[ -n "$(find "$1" -mmin "-$3" 2> /dev/null)" ] && exit 0
running=$(docker --context "$4" ps --quiet 2> /dev/null) || exit 0
[ -n "$running" ] && exit 0
exec perl -MFcntl=:flock -e "open(my \$held, \">>\", shift) or exit 1; flock(\$held, LOCK_EX | LOCK_NB) or exit 0; exec @ARGV" "$2" colima stop fennec
'
# Run on exit, by the trap below.
# shellcheck disable=SC2329
stopper() {
  touch "$used"
  [ "$context" = colima-fennec ] && command -v colima > /dev/null || return 0
  # Without the lock's descriptor, or it would hold the lock as long as it sleeps.
  eval "nohup sh -c \"\$stop_when_idle\" sh \"\$used\" \"\$lock\" 30 \"\$context\" > /dev/null 2>&1 ${FENNEC_LINUX_CHECK_HELD}>&- &"
}
trap stopper EXIT
command -v docker > /dev/null || unavailable "there's no docker"
if ! docker --context "$context" info > /dev/null 2>&1; then
  # Fennec's own VM, stopped: started, leaving Docker's context as it is.
  if [ "$context" = colima-fennec ] && command -v colima > /dev/null \
    && colima list 2> /dev/null | awk '{ print $1 }' | grep -qx fennec; then
    # Without the lock's descriptor: the VM's host agent lives on after this check, and holding it
    # would keep every later check waiting.
    eval "colima start fennec --activate=false > /dev/null 2>&1 ${FENNEC_LINUX_CHECK_HELD}>&-" \
      || unavailable "Fennec's Linux VM, colima's \`fennec\`, wouldn't start"
  fi
  docker --context "$context" info > /dev/null 2>&1 \
    || unavailable "there's no Linux VM of Fennec's own, \`$context\`: docs/landing.md says how to make one"
fi
toolchain=$(sed -n 's/^channel = "\(.*\)"$/\1/p' "$root/rust-toolchain.toml")
# The image of that release; its patch release may be newer, which builds the same.
image="rust:${toolchain%.*}-slim-bookworm"
# Names have to resolve in the VM, for crates.io and Debian's packages. After a restart on
# 2026-11-16 the VM asked a resolver Lima's user-v2 network doesn't have, and nothing resolved.
# Said plainly, as the check can still go on with the crates its volume holds.
if ! docker --context "$context" run --rm "$image" getent hosts index.crates.io > /dev/null 2>&1; then
  echo "warning: the Linux VM can't resolve index.crates.io: its /etc/resolv.conf should name 192.168.5.2, the gateway of Lima's user-v2 network, as \`dns: [192.168.5.2]\` in ~/.colima/fennec/colima.yaml has it (docs/landing.md); the check goes on with the crates it has" >&2
fi
# Past 75% of the VM's disk, the fennec- volumes no container uses go: this check's own, which
# the lock keeps for it, and other lanes' left behind. A volume a container uses, as a run of
# Miri hours long does, stays, and one that won't go is said and left: cleaning up never fails
# the check.
full=$(docker --context "$context" run --rm --volume fennec-linux-check-target:/target "$image" \
  df -P /target | awk 'NR == 2 { sub("%", "", $5); print $5 }')
if [ "${full:-0}" -gt 75 ]; then
  echo "linux-check: the VM's disk is ${full}% full, so the fennec- volumes no container uses are removed" >&2
  unused=$(docker --context "$context" volume ls --quiet --filter dangling=true | grep '^fennec-' || true)
  for volume in $unused; do
    docker --context "$context" volume rm "$volume" > /dev/null 2>&1 \
      || echo "linux-check: couldn't remove the volume $volume, so it's left" >&2
  done
fi
# What the VM holds, less an eighth for itself, in MiB.
memory=$(docker --context "$context" info --format '{{.MemTotal}}')
memory=$((memory * 7 / 8 / 1024 / 1024))m
# Inside: the checkout copied in, in place of the last one; the image's own Rust, rather than the
# exact patch release the toolchain file names, which would be downloaded every time; this kit's
# crates cleaned out; then the check.
# shellcheck disable=SC2016
inside='
find /src -mindepth 1 -delete && tar -x -f - -C /src --warning=no-unknown-keyword || exit 1
export RUSTUP_TOOLCHAIN="$(rustup default | cut -d" " -f1)"
set --
for manifest in crates/*/Cargo.toml; do
  set -- "$@" -p "$(sed -n "s/^name = \"\(.*\)\"$/\1/p" "$manifest" | head -n 1)"
done
cargo clean --locked --quiet "$@" && exec cargo check --locked --workspace --tests
'
said=$(mktemp)
status=0
# The checkout as it is, but for what's built and git's own: macOS's tar leaves out its own
# metadata, which Linux's tar has no use for.
COPYFILE_DISABLE=1 tar -c --no-xattrs --no-mac-metadata --exclude ./target --exclude ./.git -C "$root" -f - . |
docker --context "$context" run --rm --interactive --memory "$memory" \
  --volume fennec-linux-check-src:/src \
  --volume fennec-linux-check-target:/target \
  --volume fennec-linux-check-registry:/usr/local/cargo/registry \
  --env CARGO_TARGET_DIR=/target \
  --env RUSTFLAGS="-D warnings" \
  --workdir /src \
  "$image" \
  sh -c "$inside" 2>&1 | tee "$said" || status=$?
# A full disk is the VM's, not the code's.
if [ "$status" -ne 0 ] && grep -q "No space left on device" "$said"; then
  rm -f "$said"
  unavailable "Fennec's Linux VM, \`$context\`, has no disk left"
fi
rm -f "$said"
exit "$status"
