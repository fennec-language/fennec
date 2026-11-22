#!/usr/bin/env bash
# mutants-verdict.sh OUT: what a run of cargo-mutants came to, from the lists it wrote in OUT
# (`mutants.out`): how many mutants a test caught, how many none did, how many no test can since they
# change nothing (`mutants-equivalent.sh`), how many timed out and how many didn't build. A timeout
# is neither caught nor missed: the tests ran past the limit before saying. Exits 1 when most
# mutants that built timed out, since then the run checked next to nothing. Every one did for
# weeks, each run passed, and nothing said so (2026-11-20).
set -u
out=$1
count() {
  local n
  n=$(grep -c . "$out/$1.txt" 2> /dev/null) || true
  echo "${n:-0}"
}
caught=$(count caught)
missed=$(count missed)
equivalent=$(count equivalent)
timeout=$(count timeout)
unviable=$(count unviable)
built=$((caught + missed + timeout))
echo "$caught caught, $missed missed, $equivalent equivalent, $timeout timed out, $unviable didn't build"
if [ $((2 * timeout)) -gt "$built" ]; then
  echo "error: $timeout of the $built mutants that built timed out, so the run checked next to nothing: their tests take longer than the limit" >&2
  exit 1
fi
