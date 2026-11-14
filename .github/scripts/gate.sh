#!/usr/bin/env bash
# The checks a commit passes on this machine before it lands. It tests the working tree, so whatever
# runs it has to be sure that's the commit in question.
#
# Every step runs, whatever an earlier one did, so a failure main already has never hides one this
# change brings: tests run with `--no-fail-fast`, and the examples and the Rosetta pass go through
# every program. Only the steps that need the build are left out when it fails, and a step that
# didn't run fails the gate. A step's failures are named: a test by its exact name, a program by
# its path. The gate passes when every step ran and failed on nothing but what
# `.github/known-failures.txt` names, the failures main has, each with the change that broke it.
# It ends with a line for every step: whether it ran, and what it found.
set -u
root="$(git rev-parse --show-toplevel)" || exit 1
cd "$root" || exit 1
logs="$(mktemp -d)"
# Its logs, and the x86-64 programs and Fennec folder it makes, go however it ends: what a failed
# step printed is shown before then.
trap 'rm -rf "$logs"' EXIT
log="$logs/gate.log"
: > "$log"
# LLVM is the main backend, so its tests have to run, not skip.
export FENNEC_EXPECT_LLVM=1

# Each step's outcome, in order, as `name|outcome`: passed, known, failed or didn't run.
outcomes=()
# What main is known to fail, one name a line, from `.github/known-failures.txt`.
known="$logs/known.txt"
.github/scripts/known-failures.sh names > "$known" || exit 1

# Runs a step and keeps its outcome. A step may name its failures in `$names`, one a line: a step
# that fails naming only what main is known to fail is "known"; one that fails naming nothing, or
# anything else, has failed.
names="$logs/names.txt"
step() {
  local name=$1 started=$SECONDS status=0
  shift
  : > "$names"
  "$@" > "$logs/step.log" 2>&1 || status=$?
  local took=$((SECONDS - started))
  if [ "$status" -eq 0 ]; then
    echo "${took}s  $name"
    outcomes+=("$name|passed")
    return 0
  fi
  local unknown
  unknown=$(grep -vxF -f "$known" "$names" || true)
  if [ -s "$names" ] && [ -z "$unknown" ]; then
    echo "${took}s  $name: failed only on what main is known to fail:"
    sed 's/^/  /' "$names"
    outcomes+=("$name|known")
    return 0
  fi
  echo "FAILED after ${took}s: $name"
  if [ -n "$unknown" ]; then
    echo "  failures main isn't known to have:"
    printf '%s\n' "$unknown" | sed 's/^/    /'
  fi
  tail -n 60 "$logs/step.log"
  grep gate-watch "$log"
  # Kept under the code it tested: if that passes later, this was a flake, and goes in the
  # ledger. See `flakes.sh`.
  .github/scripts/flakes.sh failed "$name" "$logs/step.log"
  outcomes+=("$name|failed")
  return 1
}

# A step left out, which fails the gate as surely as one that failed.
skipped() {
  echo "didn't run: $1, $2"
  outcomes+=("$1|didn't run")
}

# A runtime library built a little at a time has come out without parts of what it uses, which a
# program then fails to link with, as if the program were at fault. So each one is checked, and one
# missing parts is built again from nothing, which makes it whole; if it still isn't, the gate stops.
#
#   whole_runtime <library> <target folder> <what cargo builds it with>...
whole_runtime() {
  local library=$1 folder=$2 manifest
  shift 2
  .github/scripts/whole-archive.sh "$library" && return 0
  echo "gate: $library was missing parts, so it's built again from nothing" >&2
  local crates=()
  for manifest in crates/*/Cargo.toml; do
    crates+=(-p "$(sed -n 's/^name = "\(.*\)"$/\1/p' "$manifest" | sed -n 1p)")
  done
  cargo clean --target-dir "$folder" "${crates[@]}" || return 1
  cargo build --locked --target-dir "$folder" "$@" || return 1
  .github/scripts/whole-archive.sh "$library"
}

runtime() {
  cargo build --locked -p fennec -p fennec-runtime || return 1
  whole_runtime target/debug/libfennec_runtime.a target -p fennec -p fennec-runtime || return 1
  # Every variant `fennec build` may take, fresh, so none is left over from another commit.
  for features in threads; do
    dir="target/$(echo "$features" | tr , -)"
    cargo build --locked -p fennec-runtime --features "$features" --target-dir "$dir" || return 1
    whole_runtime "$dir/debug/libfennec_runtime.a" "$dir" -p fennec-runtime --features "$features" || return 1
    cp "$dir/debug/libfennec_runtime.a" "target/debug/libfennec_runtime_$(echo "$features" | tr , _).a"
  done
}

# `fennec` with these arguments, within its limit, saying what was run and how it ended if it
# fails: a program killed by a signal prints nothing of its own.
fennec_ok() {
  local status=0
  .github/scripts/within.sh 120 ./target/debug/fennec "$@" || status=$?
  [ "$status" -eq 0 ] && return 0
  if [ "$status" -gt 128 ] && [ "$status" -ne 124 ]; then
    echo "\`fennec $*\` was killed by signal $((status - 128))" >&2
  else
    echo "\`fennec $*\` gave $status" >&2
  fi
  return 1
}

# Whether a program run with `FENNEC_COUNTS=1`, which said what's in `$1`, ended holding no block:
# its counts line, the last thing it writes, says `live 0`. One left live is a block nothing freed.
none_live() {
  if grep -qE ' live [1-9][0-9]*$' "$1"; then
    echo "it ended with blocks still live: $(grep -E ' live [0-9]+$' "$1" | tail -n 1)" >&2
    return 1
  fi
}

# Every example and kit through the command, as the first thing a person would do with them, each
# counting what it holds, so one that ends with a block nothing freed fails too. Each one is
# tried, and each that fails is named by its path.
examples() {
  local printed="$logs/printed.txt" said="$logs/said.txt" program kit failed=0
  for program in examples/*.fen; do
    if ! { FENNEC_COUNTS=1 fennec_ok run "$program" > "$printed" 2> "$said" \
      && none_live "$said" \
      && diff --strip-trailing-cr "${program%.fen}.out" "$printed" \
      && fennec_ok test "$program" > /dev/null; }; then
      cat "$said" >&2
      echo "$program" >> "$names"
      failed=1
    fi
  done
  for kit in examples/projects/*/; do
    [ -f "$kit/kit.toml" ] || continue
    if ! { FENNEC_COUNTS=1 fennec_ok run "$kit" > "$printed" 2> "$said" \
      && none_live "$said" \
      && diff --strip-trailing-cr "${kit%/}.out" "$printed" \
      && fennec_ok test "$kit" > /dev/null; }; then
      cat "$said" >&2
      echo "${kit%/}" >> "$names"
      failed=1
    fi
  done
  return "$failed"
}

# Every test, with `--no-fail-fast`, so a test binary that fails doesn't stop the rest. Each test
# that fails is named exactly, as `cargo test` says it; a binary that fails without naming one,
# by not building or by crashing, is named as that, which nothing known matches.
tests() {
  local said="$logs/tests.txt" status=0
  cargo test --locked --no-fail-fast > "$said" 2>&1 || status=$?
  cat "$said"
  [ "$status" -eq 0 ] && return 0
  .github/scripts/known-failures.sh tests-failed "$said" >> "$names"
  return "$status"
}

# Whether Rosetta runs a new x86-64 program. When it's wedged, every one sits unkillable instead
# of starting, so this tries a tiny one built just now, which Rosetta hasn't translated before,
# and fails the step at once, saying what to do, rather than letting each example hang.
rosetta() {
  local probe="$logs/rosetta"
  printf 'int main(void) { return 0; }\n' > "$probe.c" || return 1
  /usr/bin/cc -arch x86_64 -o "$probe" "$probe.c" || return 1
  # Rosetta's first translation of a program took 15 seconds on a busy Mac, so this waits a minute
  # by the clock: a wedged Rosetta spends no CPU time.
  if ! WITHIN_WALL=60 .github/scripts/within.sh 60 "$probe"; then
    echo "Rosetta isn't running new x86-64 programs; restart the Mac or \`sudo launchctl kickstart -k system/com.apple.oahd\`" >&2
    return 1
  fi
}

# An Arm-only pass can hide what only x86-64 does, like having two return registers, not eight. So
# `fennec` built for x86-64 builds every example as an x86-64 program, under Rosetta, with LLVM's
# own tools and a `cc` that links for x86-64, and each has to print its `.out`.
x86() {
  rosetta || return 1
  rustup target add x86_64-apple-darwin || return 1
  cargo build --locked -p fennec -p fennec-runtime --target x86_64-apple-darwin || return 1
  # And the runtime with threads, which a program that uses `launch` needs, in a folder of its
  # own as for this Mac's build, so nothing left by an earlier build of other source is taken.
  rm -f target/x86_64-apple-darwin/debug/libfennec_runtime_*.a
  cargo build --locked -p fennec-runtime --features threads --target x86_64-apple-darwin --target-dir target/threads || return 1
  cp target/threads/x86_64-apple-darwin/debug/libfennec_runtime.a target/x86_64-apple-darwin/debug/libfennec_runtime_threads.a || return 1
  local work="$logs/x86" tools program
  tools="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin"
  mkdir -p "$work/bin" || return 1
  printf '#!/bin/sh\nexec /usr/bin/cc -arch x86_64 "$@"\n' > "$work/bin/cc" && chmod +x "$work/bin/cc" || return 1
  local failed=0
  for program in examples/*.fen; do
    if ! { PATH="$work/bin:$PATH" FENNEC_HOME="$work/home" FENNEC_LLVM_BIN="$tools" \
      .github/scripts/within.sh 120 ./target/x86_64-apple-darwin/debug/fennec build "$program" -o "$work/program" \
      > /dev/null \
      && .github/scripts/within.sh 60 "$work/program" < /dev/null > "$work/printed" \
      && diff --strip-trailing-cr "${program%.fen}.out" "$work/printed"; }; then
      echo "$program" >> "$names"
      failed=1
    fi
  done
  return "$failed"
}

watched() {
  "$@" &
  local pid=$!
  .github/scripts/gate-watch.sh "$pid" "$log" &
  local watcher=$!
  wait "$pid"
  local status=$?
  wait "$watcher" 2> /dev/null
  return "$status"
}

started=$SECONDS
step "format" cargo fmt --check
built=1
step "build the command and the runtimes" runtime || built=0
step "clippy" cargo clippy --all-targets --locked -- -D warnings
step "the workspace and its tests compile on Linux" .github/scripts/linux-check.sh
step "the workspace and its tests compile for Windows" .github/scripts/windows-check.sh
for name in "tests" "examples and kits" "every example as x86-64 under Rosetta"; do
  if [ "$built" -eq 0 ]; then
    skipped "$name" "since the build failed"
    continue
  fi
  case "$name" in
    tests) step "$name" watched tests ;;
    examples*) step "$name" watched examples ;;
    *) step "$name" watched x86 ;;
  esac
done
if [ -s "$log" ]; then
  echo "the gate stopped test programs:"
  cat "$log"
  .github/scripts/flakes.sh failed "test programs the gate stopped" "$log"
  outcomes+=("test programs the gate stopped|failed")
fi

# A line for every step, and the verdict: every step ran, and failed on nothing main isn't known
# to fail.
echo
echo "the gate's steps:"
passed=1
for outcome in "${outcomes[@]}"; do
  printf '  %-12s %s\n' "${outcome##*|}" "${outcome%|*}"
  case "${outcome##*|}" in
    passed | known) ;;
    *) passed=0 ;;
  esac
done
echo "$((SECONDS - started))s  altogether"
if [ "$passed" -eq 0 ]; then
  # What gate-watch stopped, and what each was doing, printed above, kept beyond the gate's own
  # folder, which goes when it ends.
  if [ -s "$log" ]; then
    kept="${FENNEC_HOME:-$HOME/.fennec}/gate-logs"
    mkdir -p "$kept"
    kept="$kept/$(date '+%Y-%m-%d-%H%M%S')-$(git rev-parse --short HEAD).log"
    cp "$log" "$kept"
    echo "what gate-watch stopped, and what each was doing, is kept in $kept"
  fi
  echo "the gate failed"
  exit 1
fi
# Whatever failed before on this same code passed now, so each is a flake, written in the ledger.
.github/scripts/flakes.sh passed
