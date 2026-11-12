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
#   after 30 minutes with no check, so its memory isn't held around the clock. Starting it is
#   part of the step's time.
# - One check runs at a time, whichever worktree's gate starts it, under a lock this script takes
#   and runs itself again under: two building SQLite at once had the compiler killed for memory.
# - Each is held to what the VM holds, so a build too big is stopped in its own container.
# - Every worktree shares one target volume and one of fetched crates. This kit's own crates are
#   cleaned out before each check, so one worktree never takes another's build of them as fresh:
#   every checkout is mounted at the same path, and cargo goes by the times files changed.
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
# of its own sleeps that long, and stops it only if no check has run since and none holds the lock.
# Run through `eval` by the stopper below.
# shellcheck disable=SC2016,SC2034
stop_when_idle='
sleep "$(($3 * 60))"
[ -n "$(find "$1" -mmin "-$3" 2> /dev/null)" ] && exit 0
exec perl -MFcntl=:flock -e "open(my \$held, \">>\", shift) or exit 1; flock(\$held, LOCK_EX | LOCK_NB) or exit 0; exec @ARGV" "$2" colima stop fennec
'
# Run on exit, by the trap below.
# shellcheck disable=SC2329
stopper() {
  touch "$used"
  [ "$context" = colima-fennec ] && command -v colima > /dev/null || return 0
  # Without the lock's descriptor, or it would hold the lock as long as it sleeps.
  eval "nohup sh -c \"\$stop_when_idle\" sh \"\$used\" \"\$lock\" 30 > /dev/null 2>&1 ${FENNEC_LINUX_CHECK_HELD}>&- &"
}
trap stopper EXIT
command -v docker > /dev/null || unavailable "there's no docker"
if ! docker --context "$context" info > /dev/null 2>&1; then
  # Fennec's own VM, stopped: started, leaving Docker's context as it is.
  if [ "$context" = colima-fennec ] && command -v colima > /dev/null \
    && colima list 2> /dev/null | awk '{ print $1 }' | grep -qx fennec; then
    colima start fennec --activate=false > /dev/null 2>&1 \
      || unavailable "Fennec's Linux VM, colima's \`fennec\`, wouldn't start"
  fi
  docker --context "$context" info > /dev/null 2>&1 \
    || unavailable "there's no Linux VM of Fennec's own, \`$context\`: docs/landing.md says how to make one"
fi
toolchain=$(sed -n 's/^channel = "\(.*\)"$/\1/p' "$root/rust-toolchain.toml")
# The image of that release; its patch release may be newer, which builds the same.
image="rust:${toolchain%.*}-slim-bookworm"
# Past 75% of the VM's disk, the volumes this makes go, while the lock says nothing uses them.
full=$(docker --context "$context" run --rm --volume fennec-linux-check-target:/target "$image" \
  df -P /target | awk 'NR == 2 { sub("%", "", $5); print $5 }')
if [ "${full:-0}" -gt 75 ]; then
  echo "linux-check: the VM's disk is ${full}% full, so its fennec- volumes are removed and built again" >&2
  volumes=$(docker --context "$context" volume ls --quiet | grep '^fennec-' || true)
  # One name a word, as Docker names them.
  # shellcheck disable=SC2086
  [ -z "$volumes" ] || docker --context "$context" volume rm $volumes > /dev/null
fi
# What the VM holds, less an eighth for itself, in MiB.
memory=$(docker --context "$context" info --format '{{.MemTotal}}')
memory=$((memory * 7 / 8 / 1024 / 1024))m
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
status=0
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
  rm -f "$said"
  unavailable "Fennec's Linux VM, \`$context\`, has no disk left"
fi
rm -f "$said"
exit "$status"
