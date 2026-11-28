#!/usr/bin/env bash
# floor.sh: holds a nightly step to the least work it does, from `.github/floors.txt`, so a step
# that ran nothing, or a sliver of its usual, fails rather than going green. The nightly hid
# failures for four weeks behind `| tee`, and the AddressSanitizer fuzz job ran 0 inputs for three
# with nobody seeing: a green step has to prove it looked.
#
#   floor.sh KEY COUNT           one count, as `floor.sh proofs/determinism 289`
#   floor.sh --fuzz PREFIX FILE  every key `PREFIX/<target>` in floors.txt, its count the inputs
#                                FILE's `<target>: N inputs, F failures` line says, 0 with no line
#   floor.sh --tests KEY FILE    the tests `cargo test` or nextest says passed in FILE, summed
#
# Each count is a line, `floor: KEY did COUNT UNIT (at least MIN)`, on standard output, in the
# step's summary, and in `$FLOORS_SEEN` when that names a file, which the morning report reads
# from the run's log. Under its floor the line says so, with an `::error::`, and it exits 1. A key
# floors.txt doesn't have, or a count that isn't a number, exits 2: a typo can't pass as a floor.
# `NIGHTLY_ONLY` set lifts the floors, for a try by hand on a part of the night.
here=$(cd "$(dirname "$0")" && pwd)
floors=${FLOORS:-$here/../floors.txt}
[ -f "$floors" ] || { echo "floor.sh: no floors at $floors" >&2; exit 2; }

# The floor and the unit of `key`, as `MIN UNIT`, or nothing.
floor_of() {
  awk -v key="$1" '$1 == key { print $2, $3; exit }' "$floors"
}

said() {
  echo "$1"
  [ -z "${GITHUB_STEP_SUMMARY:-}" ] || echo "- ${1#floor: }" >> "$GITHUB_STEP_SUMMARY"
  [ -z "${FLOORS_SEEN:-}" ] || echo "$1" >> "$FLOORS_SEEN"
}

# Holds one count to its key's floor.
hold() {
  local key=$1 count=$2 min unit
  read -r min unit <<< "$(floor_of "$key")"
  if [ -z "$min" ]; then
    echo "floor.sh: there's no floor for \`$key\` in $floors" >&2
    return 2
  fi
  if ! [[ $count =~ ^[0-9]+$ ]]; then
    echo "::error::$key: its count \`$count\` isn't a number, so how much it did isn't known"
    said "floor: $key did an unknown number of $unit, under its floor of $min"
    return 1
  fi
  if [ -n "${NIGHTLY_ONLY:-}" ] || [ "$count" -ge "$min" ]; then
    said "floor: $key did $count $unit (at least $min)"
    return 0
  fi
  echo "::error::$key did $count $unit, under its floor of $min: the step ran less than it does on any night measured"
  said "floor: $key did $count $unit, under its floor of $min"
  return 1
}

case "${1:-}" in
  --fuzz)
    [ $# -eq 3 ] || { echo "usage: floor.sh --fuzz PREFIX FILE" >&2; exit 2; }
    prefix=$2 file=$3
    keys=$(awk -v prefix="$prefix/" 'index($1, prefix) == 1 { print $1 }' "$floors")
    [ -n "$keys" ] || { echo "floor.sh: no floors start \`$prefix/\` in $floors" >&2; exit 2; }
    status=0
    for key in $keys; do
      target=${key#"$prefix/"}
      # A target that printed no line ran nothing, as one left out does.
      count=$(sed -n "s/^$target: \([0-9][0-9]*\) inputs, [0-9][0-9]* failures\$/\1/p" "$file" 2> /dev/null | tail -n 1)
      hold "$key" "${count:-0}" || status=$?
    done
    exit "$status"
    ;;
  --tests)
    [ $# -eq 3 ] || { echo "usage: floor.sh --tests KEY FILE" >&2; exit 2; }
    # libtest's `test result: ok. N passed; …` a binary, and nextest's `N tests run: N passed`.
    count=$({ grep -oE 'test result: [A-Za-z]+\. [0-9]+ passed' "$3" | grep -oE '[0-9]+ passed' \
      ; grep -oE 'tests? run: [0-9]+ passed' "$3" | grep -oE '[0-9]+ passed'; } 2> /dev/null \
      | awk '{ total += $1 } END { print total + 0 }')
    hold "$2" "$count"
    ;;
  -*|'')
    echo "usage: floor.sh KEY COUNT | --fuzz PREFIX FILE | --tests KEY FILE" >&2
    exit 2
    ;;
  *)
    [ $# -eq 2 ] || { echo "usage: floor.sh KEY COUNT" >&2; exit 2; }
    hold "$1" "$2"
    ;;
esac
