#!/usr/bin/env bash
# Runs the programs that use threads under ThreadSanitizer, so a data race in the runtime, or in
# what it does for a program, fails the run. CI, the nightly and a run in Fennec's Linux VM all use
# it, on Linux:
#
#   .github/scripts/tsan-programs.sh <fennec>
#
# `<fennec>` has to sit beside runtime libraries built with `-Zsanitizer=thread`, plain and with
# threads, as the CI job builds them. Every program here is built with `FENNEC_SANITIZE=thread`:
# - each example that runs `parallel`, launches a task, or uses `shared` or a lock, but not the
#   network, which needs a client, and the runtime's server tests have under ThreadSanitizer;
# - the workers' fixtures;
# - the scoreboard's real programs in `bench/kernels`, `game_tick` and `chat_server` above all,
#   which are threaded: heavy `parallel`, and a hub and shards over channels;
# - `bench/realworld`'s programs.
#
# Each runs at 4 and 16 threads, and with every call forced onto the pool, and has to print what
# its `.out` says, sorted, where it has one, and end with no report. A report fails the run.
#
# Random programs from the fuzzer aren't run under it yet: the first run found a list freed twice
# by a native block's drop, which the LLVM lane is fixing, and they join once that lands.
#
# Reports are only ever suppressed by `.github/tsan-suppressions.txt`, which
# `tsan-suppressions-check.sh` holds to its rules: a runtime function by its exact name, with why.
set -uo pipefail
fennec=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
root=$(git rev-parse --show-toplevel)
cd "$root" || exit 1
.github/scripts/tsan-suppressions-check.sh || exit 1
export FENNEC_SANITIZE=thread
export TSAN_OPTIONS="halt_on_error=0:exitcode=86:report_signal_unsafe=0:suppressions=$root/.github/tsan-suppressions.txt"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0
runs=0

# What a program prints, sorted, since output from tasks comes in the order they finish: from its
# `.out`, or the hash a fixture pins instead. Nothing when there's neither.
wanted() {
  if [ -f "${1%.fen}.out" ]; then
    LC_ALL=C sort "${1%.fen}.out" | sha256sum
  elif [ -f "${1%.fen}.sorted.sha256" ]; then
    cat "${1%.fen}.sorted.sha256"
  fi
}

programs=$(grep -lE 'parallel\(|launch\(|Inbox\[|\bshared\b|\block\(' examples/*.fen | xargs grep -LE '\bNet\b')
real=""
for name in game_tick chat_server api_handler log_crunch pathfind physics grep_tool data_pipeline; do
  real="$real bench/kernels/$name.fen"
done
real="$real bench/realworld/*.fen"
# shellcheck disable=SC2086 # the globs in `$real` are meant to spread
for program in $programs crates/fennec/tests/fixtures/workers/*.fen $real; do
  # A fixture with neither an `.out` nor a pinned hash is driven by its own test.
  case "$program" in
    crates/*) [ -f "${program%.fen}.out" ] || [ -f "${program%.fen}.sorted.sha256" ] || continue ;;
  esac
  if ! .github/scripts/within.sh 300 "$fennec" build "$program" -o "$work/checked" > /dev/null 2> "$work/built.txt"; then
    echo "::error::$program didn't build under ThreadSanitizer"
    cat "$work/built.txt"
    failed=1
    continue
  fi
  # Into a file first: `grep -q` stops reading at its first match, and with `pipefail` the
  # pipe's `nm`, cut short, would fail the check.
  nm "$work/checked" > "$work/symbols.txt" 2> /dev/null || true
  if ! grep -q __tsan_read "$work/symbols.txt"; then
    echo "::error::$program was built without ThreadSanitizer"
    failed=1
    continue
  fi
  wanted "$program" > "$work/wanted.txt"
  # A program reads the files beside it, as grep_tool does, from its own folder.
  folder=$(dirname "$program")
  for threads in 4 16 stress; do
    stress=()
    count=$threads
    if [ "$threads" = stress ]; then
      stress=(FENNEC_PARALLEL_STRESS=1)
      count=4
    fi
    status=0
    (cd "$folder" && env "${stress[@]}" FENNEC_THREADS="$count" "$root/.github/scripts/within.sh" 120 "$work/checked") \
      < /dev/null > "$work/printed.txt" 2> "$work/said.txt" || status=$?
    runs=$((runs + 1))
    if grep -q "ThreadSanitizer" "$work/said.txt"; then
      echo "::error::$program at $threads threads: ThreadSanitizer reported"
      cat "$work/said.txt"
      failed=1
    elif [ "$status" -ne 0 ]; then
      echo "::error::$program at $threads threads ended with $status"
      tail -20 "$work/said.txt"
      failed=1
    elif [ -s "$work/wanted.txt" ] && [ "$(LC_ALL=C sort "$work/printed.txt" | sha256sum)" != "$(cat "$work/wanted.txt")" ]; then
      echo "::error::$program at $threads threads printed something different"
      failed=1
    fi
  done
done
echo "$runs runs under ThreadSanitizer"
[ "$runs" -ge 60 ] || { echo "::error::only $runs runs"; failed=1; }

exit "$failed"
