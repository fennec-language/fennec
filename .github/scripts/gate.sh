#!/usr/bin/env bash
# The checks a commit passes on this machine before it lands, in the order that fails fastest. It
# tests the working tree, so whatever runs it has to be sure that's the commit in question. Each
# step's time goes to standard output, and a failed step's output is printed, and stops the rest.
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

# Whether Rosetta runs a new x86-64 program. When it's wedged, every one sits unkillable instead
# of starting, so this tries a tiny one built just now, which Rosetta hasn't translated before,
# and fails the step at once, saying what to do, rather than letting each example hang.
rosetta() {
  local probe="$logs/rosetta"
  printf 'int main(void) { return 0; }\n' > "$probe.c" || return 1
  /usr/bin/cc -arch x86_64 -o "$probe" "$probe.c" || return 1
  # Rosetta's first translation of a program took 15 seconds on a busy Mac, so this waits a minute.
  if ! .github/scripts/within.sh 60 "$probe"; then
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
  for program in examples/*.fen; do
    PATH="$work/bin:$PATH" FENNEC_HOME="$work/home" FENNEC_LLVM_BIN="$tools" \
      .github/scripts/within.sh 120 ./target/x86_64-apple-darwin/debug/fennec build "$program" -o "$work/program" \
      > /dev/null || return 1
    .github/scripts/within.sh 60 "$work/program" < /dev/null > "$work/printed" || return 1
    diff --strip-trailing-cr "${program%.fen}.out" "$work/printed" || return 1
  done
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
step "every example as x86-64 under Rosetta" watched x86 || exit 1
if [ -s "$log" ]; then
  echo "the gate stopped test programs:"
  cat "$log"
  exit 1
fi
echo "$((SECONDS - started))s  altogether"
