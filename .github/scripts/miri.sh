#!/usr/bin/env bash
# The runtime's tests under Miri, which runs Rust a step at a time and stops on undefined
# behavior: a read of memory that isn't there or isn't set, a borrow broken, a data race. Our own
# crates have almost no `unsafe`, but the crates under the runtime have plenty, and Miri checks
# theirs along every path our tests take.
#
#   .github/scripts/miri.sh report.md [shard/shards]
#
# Each test runs on its own, since Miri stops a whole test binary at the first thing it can't do,
# like calling into C. So each one gets an outcome: passed, undefined behavior, deadlock, can't
# run under Miri (and why), slower than its limit, or failed. With a shard, like `3/8`, it runs
# every eighth test from the third. It writes what didn't pass to the report and exits 1 only for
# undefined behavior or a deadlock: the rest is Miri's limits, not our bugs.
#
# What Miri can't run, and what checks it instead:
# - `fennec-parked`, whose one `unsafe` follows a task's yielder pointer. corosensei, which it's
#   built on, maps a guard page and switches stacks in assembly, neither of which Miri can do.
#   CI's `parked-sanitizers` job runs its tests under AddressSanitizer, told of each switch, and
#   ThreadSanitizer, on every pull request that changes it.
# - The runtime library's own `unsafe`: its exports by name, which the linker checks, and two
#   calls into the system, `exit` and Windows' `timeBeginPeriod`, that pass a number.
# - C: SQLite, mimalloc (so the runtime is built with `sanitize`, for the system's allocator), and
#   the cryptography under TLS. Running another process, and UDP.
# - On macOS only: kqueue, which the network and task tests wait on, and a few calls that Miri has
#   for Linux only. Run on Linux, as the nightly does, those tests run.
# - Floats: Miri gives a slightly different answer for a float function whose precision Rust
#   doesn't promise, like `powi`, so a test of exact bits that relies on one fails here.
#
# MIRI_TOOLCHAIN names the nightly (the sanitizers' unless set), MIRI_SECONDS the CPU time each
# test gets (240 unless set), MIRI_SEED a seed for Miri's choices of thread order and addresses,
# and MIRI_ONLY a pattern a test's name must match, as `grep -E` reads it, to try a few.
set -u
report=$1
shard=${2:-1/1}
toolchain=${MIRI_TOOLCHAIN:-nightly-2026-10-15}
seconds=${MIRI_SECONDS:-240}
only=${MIRI_ONLY:-}
first=${shard%/*}
shards=${shard#*/}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# Isolation off, for the tests that read files and the clock. Leaks aren't undefined behavior, and
# a test may leave a thread waiting forever, or leak on purpose, so Miri isn't told to stop on them.
flags="-Zmiri-disable-isolation -Zmiri-ignore-leaks"
[ -n "${MIRI_SEED:-}" ] && flags="$flags -Zmiri-seed=$MIRI_SEED"
export MIRIFLAGS="$flags"
miri() { cargo "+$toolchain" miri "$@"; }

# What runs: each package and test target, with the runtime's features.
targets=(
  "fennec-rt --lib"
  "fennec-runtime --lib"
  "fennec-runtime --test allocator"
)
miri setup > /dev/null || exit 1
for target in "${targets[@]}"; do
  # One word a field, as cargo reads them.
  # shellcheck disable=SC2086
  miri test --locked -q -p $target --features sanitize --no-run > /dev/null 2>&1 \
    || { echo "::error::$target didn't build for Miri"; miri test --locked -p $target --features sanitize --no-run; exit 1; }
  # A test marked ignored stays ignored here too.
  # shellcheck disable=SC2086
  miri test --locked -q -p $target --features sanitize -- --list --ignored 2> /dev/null \
    | sed -n 's/: test$//p' | sort > "$work/ignored"
  # shellcheck disable=SC2086
  miri test --locked -q -p $target --features sanitize -- --list 2> /dev/null \
    | sed -n 's/: test$//p' | sort | comm -23 - "$work/ignored" \
    | sed "s/^/$target|/" >> "$work/all"
done
[ -n "$only" ] && { grep -E -e "$only" "$work/all" > "$work/only"; mv "$work/only" "$work/all"; }
awk -v first="$first" -v shards="$shards" '(NR - 1) % shards == first - 1' "$work/all" > "$work/mine"
total=$(wc -l < "$work/mine" | tr -d ' ')
echo "Miri: $total tests in shard $shard, $seconds s of CPU each, with $MIRIFLAGS"
# None means the tests weren't listed, or MIRI_ONLY matched none: either way nothing was checked.
[ "$total" -gt 0 ] || { echo "::error::no tests to run under Miri in shard $shard"; exit 1; }

# The first line of a test's log that says why, without its `error: `.
why() { grep -m1 -E "$1" "$log" | sed -E 's/^ *(error: )?//' | cut -c1-200; }
# What a run of Miri that wrote `$log` and exited with `$1` came to, in `outcome` and `reason`.
classify() {
  local code=$1
  # Past its CPU time, Miri is stopped by SIGXCPU and cargo says so; past the clock, `within.sh`
  # stops it and exits 124.
  if [ "$code" -eq 124 ] || grep -q SIGXCPU "$log"; then
    outcome=slow reason="over $seconds s of CPU"
  # An exception, until rustix is upgraded: rustix 1.1.6 calls `assume_init` on a `stat` whose
  # spare fields the system isn't bound to set, which Miri's `fstatat` for macOS leaves unset, as
  # https://github.com/rust-lang/miri/issues/5553 settled it should. A Mac's kernel does zero them,
  # so no real run reads garbage. https://github.com/bytecodealliance/rustix/pull/1737 fixes it in
  # rustix, not yet released; https://github.com/fennec-language/fennec/issues/1773 takes this out
  # once a release with it is in Cargo.lock. On Linux, Miri has no `fstatat64` at all.
  elif grep -q 'apple::stat: at .st_[lq]spare' "$log"; then
    outcome=unsupported reason="rustix 1.1.6 assumes stat's spare fields are set (fennec#1773)"
  elif grep -q 'Undefined Behavior' "$log"; then
    outcome=UB reason=$(why 'Undefined Behavior')
  elif grep -q 'deadlocked' "$log"; then
    outcome=deadlock reason=$(why 'deadlock')
  elif [ "$code" -eq 0 ] && grep -q 'test result: ok. 1 passed' "$log"; then
    outcome=passed reason=
  elif grep -q 'unsupported operation' "$log"; then
    outcome=unsupported reason=$(why 'unsupported operation' | sed 's/^unsupported operation: //')
  else
    # Where it panicked and the line after, which says how; or else the first error.
    reason=$(grep -m1 -A1 "panicked at" "$log" | sed -E "s/^thread .[^']*. \([0-9]+\) //" | paste -sd " " - | cut -c1-200)
    outcome=failed reason=${reason:-$(why "^error")}
  fi
}
log="$work/log"
# It checks itself first: a read of freed memory has to be found, and called undefined behavior.
mkdir -p "$work/check/src"
printf '[package]\nname = "check"\nversion = "0.0.0"\nedition = "2024"\n[workspace]\n' > "$work/check/Cargo.toml"
printf 'fn main() {\n    let freed = vec![1_u8];\n    let at = freed.as_ptr();\n    drop(freed);\n    println!("{}", unsafe { *at });\n}\n' > "$work/check/src/main.rs"
(cd "$work/check" && cargo "+$toolchain" miri run -q) > "$log" 2>&1
classify $?
if [ "$outcome" != UB ]; then
  echo "::error::Miri didn't find a read of freed memory, or this script didn't see it: it said $outcome"
  cat "$log"
  exit 1
fi
passed=0 bad=0
: > "$work/outcomes"
while IFS='|' read -r target name; do
  # shellcheck disable=SC2086
  WITHIN_WALL=$((seconds * 3)) .github/scripts/within.sh "$seconds" \
    cargo "+$toolchain" miri test --locked -q -p $target --features sanitize -- --exact "$name" > "$log" 2>&1
  code=$?
  classify "$code"
  echo "$outcome|$target|$name|$reason" >> "$work/outcomes"
  case "$outcome" in
    passed) passed=$((passed + 1)) ;;
    UB | deadlock)
      bad=$((bad + 1))
      echo "::error::$name: $reason"
      # The whole of what Miri said, with where it happened, for a repro.
      sed -E -n '/Undefined Behavior|deadlock/,/^error: aborting/p' "$log" | head -80
      ;;
  esac
  echo "$outcome $name"
done < "$work/mine"

{
  echo "## Miri, shard $shard"
  echo
  echo "$passed of $total tests passed, with \`$MIRIFLAGS\`."
  for outcome in UB deadlock failed slow unsupported; do
    count=$(grep -c "^$outcome|" "$work/outcomes")
    [ "$count" -eq 0 ] && continue
    echo
    echo "### $outcome: $count"
    echo
    grep "^$outcome|" "$work/outcomes" | while IFS='|' read -r _ target name reason; do
      echo "- \`${target%% *}\` \`$name\`${reason:+: $reason}"
    done
  done
} >> "$report"
# Listed isn't checked: a shard whose tests all failed, ran out of time or weren't supported by
# Miri checked nothing, so it's held to the tests that passed. MIRI_ONLY, a try by hand, isn't.
held=0
[ -n "$only" ] || "$(dirname "$0")/floor.sh" miri/passed "$passed" || held=1
[ "$bad" -eq 0 ] && [ "$held" -eq 0 ]
