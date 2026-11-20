#!/usr/bin/env bash
# Tries `mutants-ranked.sh` and `mutants-verdict.sh` on lists like those cargo-mutants writes: the
# counts are each said, a timeout counts as neither caught nor missed, and a run where most mutants
# that built timed out fails, since it checked next to nothing.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap '/bin/rm -rf "$work"' EXIT
failures=0

check() {
  if [ "$2" = "$3" ]; then
    echo "ok: $1"
  else
    echo "FAILED: $1: expected '$2', got '$3'"
    failures=$((failures + 1))
  fi
}

# A run's lists, each `<list>=<count>`, as `crates/ir/src/lower.rs:<n>:9: ...` lines.
lists() {
  local out=$work/$1
  shift
  /bin/rm -rf "$out"
  mkdir -p "$out"
  for list in "$@"; do
    local name=${list%=*} n=${list#*=}
    : > "$out/$name.txt"
    local i=0
    while [ "$i" -lt "$n" ]; do
      i=$((i + 1))
      echo "crates/ir/src/lower.rs:$((900 + i)):9: replace && with || in Lowering::assign" >> "$out/$name.txt"
    done
  done
}
verdict() {
  "$here/mutants-verdict.sh" "$work/$1" > "$work/said" 2>&1
  echo $?
}
said() {
  if grep -qF -- "$1" "$work/said"; then echo yes; else echo no; fi
}
ranked() {
  "$here/mutants-ranked.sh" "$work/$1" fennec-ir 14/60 > "$work/said" 2>&1
}

lists mixed caught=3 missed=2 timeout=1 unviable=4
check "a run with few timeouts passes" 0 "$(verdict mixed)"
check "it says each count" yes "$(said "3 caught, 2 missed, 1 timed out, 4 didn't build")"
ranked mixed
check "the slice says each count, a timeout apart from caught" yes "$(said "10 mutants tried: 3 caught, 2 missed, 1 timed out, 4 didn't build.")"
check "it lists what was missed by file" yes "$(said "- \`crates/ir/src/lower.rs\`: 2")"
check "it doesn't say most timed out" no "$(said "Most of those that built timed out")"

lists hung caught=1 missed=1 timeout=3 unviable=5
check "a run where most that built timed out fails" 1 "$(verdict hung)"
check "it says why" yes "$(said "3 of the 5 mutants that built timed out")"
ranked hung
check "the slice says most timed out" yes "$(said "Most of those that built timed out")"

lists empty caught=0 missed=0 timeout=0 unviable=0
check "empty lists pass" 0 "$(verdict empty)"
check "empty lists count as none, once each" yes "$(said "0 caught, 0 missed, 0 timed out, 0 didn't build")"
ranked empty
check "the slice says empty lists count as none" yes "$(said "0 mutants tried: 0 caught, 0 missed, 0 timed out, 0 didn't build.")"

lists none
check "a run that wrote no lists passes" 0 "$(verdict none)"
check "it counts none of each" yes "$(said "0 caught, 0 missed, 0 timed out, 0 didn't build")"

if [ "$failures" -ne 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "every check passed"
