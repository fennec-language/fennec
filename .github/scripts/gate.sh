#!/usr/bin/env bash
# The checks a commit passes on this machine before it lands. It tests the working tree, so whatever
# runs it has to be sure that's the commit in question.
#
# The quick checks come first: formatting, the lints, the ratchets and the pinned counts, which
# take a minute or two where the rest take an hour. One that fails stops the gate there, since
# what it found is a fix to make before an hour of tests is worth spending.
#
# After them, every step runs, whatever an earlier one did, so a failure main already has never
# hides one this change brings: tests run with `--no-fail-fast`, and the examples and the Rosetta
# pass go through every program. Only the steps that need the build are left out when it fails,
# and a step that didn't run fails the gate. A step's failures are named: a test by its exact name, a program by
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
# Every executable a build makes as a new file, a line each, which macOS may hold at its first
# launch: counted for each step at the end.
export FENNEC_NEW_EXECUTABLES="$logs/new-executables.txt"
: > "$FENNEC_NEW_EXECUTABLES"
# How many each step made, as `name|count`.
made=()

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
  local name=$1 started=$SECONDS status=0 before
  shift
  : > "$names"
  before=$(wc -l < "$FENNEC_NEW_EXECUTABLES")
  "$@" > "$logs/step.log" 2>&1 || status=$?
  local took=$((SECONDS - started))
  made+=("$name|$(($(wc -l < "$FENNEC_NEW_EXECUTABLES") - before))")
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
  for features in threads audit threads,audit; do
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
  # The oracle (crates/oracle) runs each example a second way, from its source. One it runs has
  # to print what the example's `.out` says and end with 0; one it doesn't model, which it says
  # by ending with 3, is skipped. A difference is named `oracle:<path>`.
  cargo build --locked --quiet -p fennec-oracle || return 1
  for program in examples/*.fen; do
    if ! oracle_agrees "$program" "${program%.fen}.out"; then
      echo "oracle:$program" >> "$names"
      failed=1
    fi
  done
  return "$failed"
}

# Whether the oracle, run on the program `$1`, agrees with the output `$2` it should print: it
# prints that and ends with 0, or it doesn't model the program and ends with 3.
oracle_agrees() {
  local status=0 printed="$logs/oracle.txt"
  .github/scripts/within.sh 120 ./target/debug/fennec-oracle run "$1" > "$printed" 2> "$logs/oracle-said.txt" \
    || status=$?
  [ "$status" -eq 3 ] && return 0
  if [ "$status" -ne 0 ]; then
    echo "the oracle ran $1 and it ended with $status:" >&2
    cat "$logs/oracle-said.txt" >&2
    return 1
  fi
  diff --strip-trailing-cr "$2" "$printed" >&2 || { echo "the oracle ran $1 and printed something else" >&2; return 1; }
}

# What takes a minute or two, rather than the hour the rest takes, and has failed gates an hour
# in: each check runs, and what fails is named.
# - The Fennec code is formatted: the examples, the standard library and the benchmarks.
# - The examples pass the linter.
# - The uses of `unsafe` are what `.github/unsafe-allowed.txt` lists.
# - The ratchet of unchecked runtime calls, `crates/llvm/unchecked_runtime_calls.txt`, holds.
# - Each benchmark kernel makes exactly the copies `bench/copies.txt` says.
# - The ThreadSanitizer suppressions keep to their rules, and the known failures' file to its form.
# - The workflows pass actionlint and zizmor, where they're installed.
quick() {
  local failed=0 program said="$logs/quick.txt"
  if ! ./target/debug/fennec fmt --check examples crates/syntax/src/stdlib bench; then
    echo "fennec fmt --check" >> "$names"
    failed=1
  fi
  for program in examples/*.fen examples/projects/*/; do
    [ -f "$program" ] || [ -f "$program/kit.toml" ] || continue
    if ! .github/scripts/within.sh 60 ./target/debug/fennec lint "$program" > /dev/null; then
      echo "lint:$program" >> "$names"
      failed=1
    fi
  done
  # As CI's lint job counts them: every use outside a comment, by file, in one perl.
  git ls-files -z '*.rs' | perl -e 'my @files = split /\0/, do { local $/; <STDIN> }; my %c;
    for my $file (@files) { open(my $f, "<", $file) or die "$file: $!";
      while (<$f>) { next if m{^\s*//}; $c{$file} += () = /\bunsafe\b/g } }
    printf "%s: %d\n", $_, $c{$_} for grep { $c{$_} } sort keys %c' > "$logs/unsafe.txt"
  if ! diff -u <(grep -v '^#' .github/unsafe-allowed.txt) "$logs/unsafe.txt"; then
    echo "the uses of unsafe changed: update .github/unsafe-allowed.txt, and say why" >&2
    echo "the uses of unsafe" >> "$names"
    failed=1
  fi
  if ! cargo test --locked -p fennec-llvm --lib -- failure_checks:: > "$said" 2>&1 \
    || ! cargo test --locked -p fennec --test fennec -- --exact examples::benchmark_kernels_copy_what_was_pinned >> "$said" 2>&1; then
    cat "$said"
    .github/scripts/known-failures.sh tests-failed "$said" >> "$names"
    failed=1
  fi
  .github/scripts/tsan-suppressions-check.sh || { echo "tsan-suppressions-check" >> "$names"; failed=1; }
  .github/scripts/test-known-failures.sh > /dev/null || { echo "test-known-failures" >> "$names"; failed=1; }
  if command -v actionlint > /dev/null && ! actionlint; then
    echo "actionlint" >> "$names"
    failed=1
  fi
  if command -v zizmor > /dev/null && ! zizmor --offline --min-severity low -q .github/workflows; then
    echo "zizmor" >> "$names"
    failed=1
  fi
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
  .github/scripts/gate-watch.sh "$pid" "$log" "" "" "" "$logs/holds.txt" &
  local watcher=$!
  wait "$pid"
  local status=$?
  wait "$watcher" 2> /dev/null
  return "$status"
}

started=$SECONDS
# A quick check that fails, on anything main isn't known to fail, stops the gate after it.
stopped=0
step "format" cargo fmt --check || stopped=1
built=1
step "build the command and the runtimes" runtime || built=0
if [ "$built" -eq 1 ]; then
  step "the quick checks" quick || stopped=1
else
  skipped "the quick checks" "since the build failed"
fi
for name in "clippy" "the workspace and its tests compile on Linux" "the workspace and its tests compile for Windows" \
  "tests" "examples and kits" "every example as x86-64 under Rosetta"; do
  if [ "$stopped" -eq 1 ]; then
    skipped "$name" "since a quick check failed: fix it, and run the gate again"
    continue
  fi
  case "$name" in
    clippy) step "$name" cargo clippy --all-targets --locked -- -D warnings ;;
    *Linux) step "$name" .github/scripts/linux-check.sh ;;
    *Windows) step "$name" .github/scripts/windows-check.sh ;;
    *)
      if [ "$built" -eq 0 ]; then
        skipped "$name" "since the build failed"
        continue
      fi
      case "$name" in
        tests) step "$name" watched tests ;;
        examples*) step "$name" watched examples ;;
        *) step "$name" watched x86 ;;
      esac
      ;;
  esac
done
if [ -s "$log" ]; then
  echo "the gate stopped test programs:"
  cat "$log"
  .github/scripts/flakes.sh failed "test programs the gate stopped" "$log"
  outcomes+=("test programs the gate stopped|failed")
fi

# How many new executables each step made.
echo "new executables made: $(wc -l < "$FENNEC_NEW_EXECUTABLES" | tr -d ' ')"
for each in "${made[@]}"; do
  [ "${each##*|}" -gt 0 ] && printf '  %6d  %s\n' "${each##*|}" "${each%|*}"
done
# How long macOS held brand-new programs at launch, which counts against no limit, but is time
# the gate waited.
if [ -s "$logs/holds.txt" ]; then
  sort -rn "$logs/holds.txt" | awk '
    { count++; total += $1; if (NR <= 5) longest[NR] = $0 }
    END {
      printf "%d test programs were held at launch, %ds altogether; the longest:\n", count, total
      for (at = 1; at <= NR && at <= 5; at++) printf "  %ss  %s\n", substr(longest[at], 1, index(longest[at], " ") - 1), substr(longest[at], index(longest[at], " ") + 1, 120)
    }'
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
