#!/usr/bin/env bash
# Every program runs the same however it's run. Each example, kit, test fixture with an `.out`,
# and a share of random programs is built once, then run four ways, and each run must print and
# exit as the first did, byte for byte:
#
#   seed 1, 1 thread               the first run
#   seed 2, many threads, loaded   another hash seed, more workers, and every core kept busy
#   recorded                       `fennec run --record`: its outside inputs written to a log
#   replayed                       `fennec replay`: run again from that log
#
# The tests of each example and kit run twice as well, on 1 thread and on many, loaded, and must
# say the same. A program whose output is meant to vary, like one printing the time, is named in
# `determinism-varies.txt` with why; it's still recorded and replayed, which must agree.
#
#   .github/scripts/determinism.sh target/release/fennec report.md [random programs]
#
# The third argument, 50 unless given, is how many random programs `fennec-fuzz show programs`
# writes beside `fennec`. Every difference is a line of the report naming the program, how each
# run was made, and what each printed, and it exits 1 if there was one. `NIGHTLY_ONLY` limits it
# to programs whose path matches, as `case` does.
set -u
fennec=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
fuzz=$(dirname "$fennec")/fennec-fuzz
report=$2
randoms=${3:-50}
work=$(mktemp -d)
burners=()
trap 'kill ${burners[@]+"${burners[@]}"} 2> /dev/null; rm -rf "$work"' EXIT
within=$PWD/.github/scripts/within.sh
varies=.github/scripts/determinism-varies.txt
many=$(($(getconf _NPROCESSORS_ONLN) * 2))
different=0
checked=0
said() {
  echo "$1" >> "$report"
  echo "$1"
}
# How `determinism-varies.txt` names `$1`: `sorted` when only the order of its lines may vary, as
# what `parallel` bodies print does, `varies` when its output is meant to vary, or nothing.
named() {
  awk -v name="$1" '$1 == name { print ($2 == "sorted" ? "sorted" : "varies"); exit }' "$varies" 2> /dev/null
}
# Whether runs `$1` and `$2` of a program named `$3` printed the same: sorted, if it's named so.
same() {
  if [ "$3" = sorted ]; then
    cmp -s <(sort "$1") <(sort "$2")
  else
    cmp -s "$1" "$2"
  fi
}
# Keeps every core busy until `unload`, so a run's timing is a loaded machine's.
load() {
  local _
  for _ in $(seq "$(getconf _NPROCESSORS_ONLN)"); do
    (while :; do :; done) &
    burners+=("$!")
  done
}
unload() {
  kill ${burners[@]+"${burners[@]}"} 2> /dev/null
  wait ${burners[@]+"${burners[@]}"} 2> /dev/null
  burners=()
}
# Runs `$1` in folder `$2` with the environment in `$4`..., keeping its output as `$3`: what it
# printed, then the line `exit <code>`.
run() {
  local executable=$1 folder=$2 kept=$3 code=0
  shift 3
  (cd "$folder" && env "$@" "$within" 300 "$executable" < /dev/null > "$kept" 2> "$kept.said") || code=$?
  echo "exit $code" >> "$kept"
}
# Says that `$1`'s runs `$2` and `$4` differ, with what each, kept in `$3` and `$5`, printed.
differs() {
  echo "::error::$1 runs differently: $2, then $4"
  {
    echo "- DIFFERENT: \`$1\`, run $2, then $4."
    echo '  ```diff'
    diff "$3" "$5" | head -20 | sed 's/^/  /'
    echo '  ```'
  } >> "$report"
  different=1
}
# Runs `fennec` with the arguments after `$1`, keeping what it printed and how it exited as `$1`,
# as `run` does.
fennec_run() {
  local kept=$1 code=0
  shift
  "$within" 300 "$fennec" "$@" < /dev/null > "$kept" 2> "$kept.said" || code=$?
  echo "exit $code" >> "$kept"
}
# Checks one program, `$1`, built as `$2`, four ways, against its first run. `fennec run --record`
# starts the log the program writes its inputs to, and `fennec replay` runs it from that log.
check() {
  local name=$1 executable=$2 folder=$3 source=$4
  checked=$((checked + 1))
  run "$executable" "$folder" "$work/first" FENNEC_HASH_SEED=1 FENNEC_THREADS=1
  load
  run "$executable" "$folder" "$work/loaded" FENNEC_HASH_SEED=2 FENNEC_THREADS="$many"
  unload
  rm -f "$work/log"
  fennec_run "$work/recorded" run --record "$work/log" "$source"
  fennec_run "$work/replayed" replay "$work/log"
  local how
  how=$(named "$name")
  if [ "$how" != varies ]; then
    same "$work/first" "$work/loaded" "$how" ||
      differs "$name" "with seed 1 on 1 thread" "$work/first" "with seed 2 on $many threads, loaded" "$work/loaded"
    same "$work/first" "$work/recorded" "$how" ||
      differs "$name" "with seed 1 on 1 thread" "$work/first" "recorded" "$work/recorded"
  fi
  cmp -s "$work/recorded" "$work/replayed" ||
    differs "$name" "recorded" "$work/recorded" "replayed from its log" "$work/replayed"
}
# Builds `$1` and checks it, as `$1` itself, in folder `$2`.
built() {
  local program=$1 folder=$2
  # shellcheck disable=SC2254
  case "$program" in ${NIGHTLY_ONLY:-*}) ;; *) return ;; esac
  "$within" 300 "$fennec" build "$program" -o "$work/program" > /dev/null 2> "$work/built.txt" || return
  check "$program" "$work/program" "$folder" "$program"
}
# Runs the tests of `$1` on 1 thread and on many, loaded, which must say the same.
tested() {
  local program=$1
  # shellcheck disable=SC2254
  case "$program" in ${NIGHTLY_ONLY:-*}) ;; *) return ;; esac
  local one=0 loaded=0
  # With no cache, so the second run tests again rather than reusing the first's results.
  FENNEC_NO_CACHE=1 FENNEC_THREADS=1 "$within" 300 "$fennec" test "$program" < /dev/null > "$work/tests-one" 2>&1 || one=$?
  load
  FENNEC_NO_CACHE=1 FENNEC_THREADS=$many "$within" 300 "$fennec" test "$program" < /dev/null > "$work/tests-loaded" 2>&1 || loaded=$?
  unload
  echo "exit $one" >> "$work/tests-one"
  echo "exit $loaded" >> "$work/tests-loaded"
  cmp -s "$work/tests-one" "$work/tests-loaded" ||
    differs "$program's tests" "on 1 thread" "$work/tests-one" "on $many threads, loaded" "$work/tests-loaded"
}

echo "## Determinism" >> "$report"
for program in examples/*.fen; do
  built "$program" .
  tested "$program"
done
for kit in examples/projects/*/; do
  [ -f "$kit/kit.toml" ] || continue
  built "${kit%/}" .
  tested "${kit%/}"
done
for program in crates/fennec/tests/fixtures/*.fen; do
  [ -f "${program%.fen}.out" ] && built "$program" .
done
if [ -z "${NIGHTLY_ONLY:-}" ] && [ -x "$fuzz" ]; then
  for n in $(seq "$randoms"); do
    head -c 1200 /dev/urandom > "$work/input"
    "$fuzz" show programs "$work/input" > "$work/random.fen" 2> /dev/null || continue
    if "$within" 300 "$fennec" build "$work/random.fen" -o "$work/program" > /dev/null 2>&1; then
      before=$different
      different=0
      check "random program $n" "$work/program" . "$work/random.fen"
      # A random program that ran differently is kept, for the run's artifact to hold.
      if [ "$different" = 1 ]; then
        mkdir -p found/determinism
        cp "$work/input" "found/determinism/random-$n"
        said "  Its input is kept as \`found/determinism/random-$n\`, for \`fennec-fuzz show programs\`."
      fi
      [ "$before" = 1 ] && different=1
    fi
  done
fi
# The programs named as meant to vary, so the report shows the list as it stands.
while read -r name _; do
  [ -n "$name" ] && [ "${name#\#}" = "$name" ] && said "- \`$name\` is named in determinism-varies.txt: $(named "$name")."
done < "$varies"
said "- $checked programs ran four ways each$([ "$different" = 0 ] && echo ", all the same")."
exit "$different"
