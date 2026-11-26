#!/usr/bin/env bash
# Runs `cargo test` with its test binaries two at a time, rather than one after another, which is
# how `cargo test` runs them. In CI's `others` part, a run spent 28 minutes running binaries, a few
# of which hold the run for minutes with only a handful of long tests and cores to spare.
#
#   .github/scripts/two-at-a-time.sh --locked --no-fail-fast --workspace --exclude fennec
#
# takes `cargo test`'s arguments. Cargo builds and starts each binary as ever, with the folder and
# the environment it gives one, but through this script as its runner, which waits for one of two
# places, starts the binary there in the background, and hands back at once. Each binary's output
# goes to a file of its own, printed whole, in cargo's order, once all have finished. Like
# `--no-fail-fast`, every binary runs, and this fails if any did, or if cargo did.
#
# Doc tests aren't run through a runner, so cargo runs them itself, one after another, as before.
set -uo pipefail

# Cargo calls this with the binary and its arguments, as each binary's runner.
if [ -n "${TWO_AT_A_TIME_PLACES:-}" ]; then
  places=$TWO_AT_A_TIME_PLACES
  number=$(($(find "$places/started" -type f | wc -l) + 1))
  # The test's name, as `cargo test` says it before running one: its file, from its path.
  echo "$number $*" > "$places/started/$number"
  until mkdir "$places/place-1" 2> /dev/null && place=1 || { mkdir "$places/place-2" 2> /dev/null && place=2; }; do
    sleep 0.2
  done
  # In the background, and without this script's place in its environment, so a test that runs
  # `cargo` itself runs it as ever: the runner is given on cargo's command line, which no test
  # inherits.
  (
    env -u TWO_AT_A_TIME_PLACES "$@" > "$places/said-$number" 2>&1
    echo $? > "$places/status-$number"
    rmdir "$places/place-$place"
  ) < /dev/null &
  exit 0
fi

places=$(mktemp -d)
trap 'rm -rf "$places"' EXIT
mkdir "$places/started"
# The runner for the platform cargo builds for: this script, run by the bash running it now, each
# named whole, since on Windows they're `C:/Program Files/...` and `bash` alone may be another.
target=$(rustc -vV | sed -n 's/^host: //p')
script=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
shell=$BASH
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) script=$(cygpath -m "$script") shell=$(cygpath -m "$BASH") ;; esac

TWO_AT_A_TIME_PLACES="$places" cargo --config "target.$target.runner = [\"$shell\", \"$script\"]" test "$@"
cargo_status=$?

# Every binary started has finished when each has a status.
started=$(($(find "$places/started" -type f | wc -l)))
while [ "$(find "$places" -maxdepth 1 -name 'status-*' | wc -l)" -lt "$started" ]; do
  sleep 1
done

failed=0
for number in $(seq 1 "$started"); do
  read -r _ binary _ < "$places/started/$number"
  binary=${binary##*[/\\]}
  status=$(cat "$places/status-$number")
  echo "::group::$binary ($status)"
  cat "$places/said-$number"
  echo "::endgroup::"
  if [ "$status" != 0 ]; then
    echo "::error::$binary failed, with $status: its output is in the group above"
    failed=1
  fi
done
echo "$started test binaries ran, two at a time"
[ "$cargo_status" = 0 ] && [ "$failed" = 0 ]
