#!/usr/bin/env bash
# The checks a commit passes on this machine before it lands, in the order that fails fastest. It
# tests the working tree, so whatever runs it has to be sure that's the commit in question. Each
# step's time goes to standard output, and a failed step's output is printed, and stops the rest.
set -u
root="$(git rev-parse --show-toplevel)" || exit 1
cd "$root" || exit 1
logs="$(mktemp -d)"
log="$logs/gate.log"
: > "$log"
# LLVM is the main backend, so its tests have to run, not skip.
export FENNEC_EXPECT_LLVM=1

step() {
  local name=$1 started=$SECONDS
  shift
  if ! "$@" > "$logs/step.log" 2>&1; then
    echo "FAILED after $((SECONDS - started))s: $name"
    tail -n 60 "$logs/step.log"
    grep gate-watch "$log"
    return 1
  fi
  echo "$((SECONDS - started))s  $name"
}

runtime() {
  cargo build --locked -p fennec -p fennec-runtime || return 1
  cargo build --locked -p fennec-runtime --features threads --target-dir target/threads || return 1
  cp target/threads/debug/libfennec_runtime.a target/debug/libfennec_runtime_threads.a
}

# Every example and kit through the command, as the first thing a person would do with them.
examples() {
  local printed="$logs/printed.txt" program kit
  for program in examples/*.fen; do
    .github/scripts/within.sh 120 ./target/debug/fennec run "$program" > "$printed" || return 1
    diff --strip-trailing-cr "${program%.fen}.out" "$printed" || return 1
    .github/scripts/within.sh 120 ./target/debug/fennec test "$program" > /dev/null || return 1
  done
  for kit in examples/projects/*/; do
    [ -f "$kit/kit.toml" ] || continue
    .github/scripts/within.sh 120 ./target/debug/fennec run "$kit" > "$printed" || return 1
    diff --strip-trailing-cr "${kit%/}.out" "$printed" || return 1
    .github/scripts/within.sh 120 ./target/debug/fennec test "$kit" > /dev/null || return 1
  done
}

# An Arm-only pass can hide what only x86-64 does, like having two return registers, not eight.
x86() {
  rustup target add x86_64-apple-darwin || return 1
  cargo test --locked -p fennec --target x86_64-apple-darwin the_jit_runs_examples_exactly
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
step "format" cargo fmt --check || exit 1
step "build the command and the runtimes" runtime || exit 1
step "clippy" cargo clippy --all-targets --locked -- -D warnings || exit 1
step "tests" watched cargo test --locked || exit 1
step "examples and kits" watched examples || exit 1
step "the JIT as x86-64 under Rosetta" watched x86 || exit 1
if [ -s "$log" ]; then
  echo "the gate stopped test programs:"
  cat "$log"
  exit 1
fi
echo "$((SECONDS - started))s  altogether"
