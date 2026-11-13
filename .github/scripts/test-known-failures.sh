#!/usr/bin/env bash
# Tries known-failures.sh on `cargo test` output: tests that fail are named exactly, and a test
# binary that crashes or doesn't build is named as that; and on the file's form.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() {
  echo "FAILED: $*"
  failures=$((failures + 1))
}

# Two binaries: one with two tests failing, one passing.
cat > "$work/two.txt" <<'OUT'
     Running unittests src/lib.rs (target/debug/deps/fennec_blake3-0123)
running 3 tests
test tests::one ... FAILED
test tests::two ... ok
test tests::three ... FAILED

failures:

---- tests::one stdout ----
thread 'tests::one' panicked at src/lib.rs:9:5:

---- tests::three stdout ----
thread 'tests::three' panicked at src/lib.rs:19:5:

failures:
    tests::one
    tests::three

test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p fennec-blake3 --lib`
     Running tests/main.rs (target/debug/deps/main-4567)
running 1 test
test cli::works ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
OUT
[ "$("$here/known-failures.sh" tests-failed "$work/two.txt")" = "$(printf 'tests::one\ntests::three')" ] \
  || fail "two failing tests weren't named exactly: $("$here/known-failures.sh" tests-failed "$work/two.txt")"

# A binary that crashed, naming no test, beside one that named its own.
cat "$work/two.txt" > "$work/crash.txt"
cat >> "$work/crash.txt" <<'OUT'
     Running tests/llvm.rs (target/debug/deps/llvm-89ab)
running 4 tests
error: test failed, to rerun pass `-p fennec --test llvm`

Caused by:
  process didn't exit successfully: `target/debug/deps/llvm-89ab` (signal: 11, SIGSEGV: invalid memory reference)
OUT
"$here/known-failures.sh" tests-failed "$work/crash.txt" | grep -qx "a test binary that failed without naming a test" \
  || fail "a binary that crashed wasn't named"

# A test that didn't build. The backticks are cargo's own.
# shellcheck disable=SC2016
printf 'error: could not compile `fennec` (test "fennec") due to 1 previous error\n' > "$work/build.txt"
"$here/known-failures.sh" tests-failed "$work/build.txt" | grep -qx "a test that didn't build" \
  || fail "a test that didn't build wasn't named"

# Every name the file holds, and the file's form, checked on a copy.
root=$(git -C "$here" rev-parse --show-toplevel)
copy="$work/repo"
mkdir -p "$copy/.github/scripts"
cp "$here/known-failures.sh" "$copy/.github/scripts/"
git -C "$copy" init -q
printf '# a comment\ntests::one | #1609\nsrc/lib.rs - hash (line 3) | a1b2c3d\n' > "$copy/.github/known-failures.txt"
[ "$(cd "$copy" && .github/scripts/known-failures.sh names)" = "$(printf 'tests::one\nsrc/lib.rs - hash (line 3)')" ] \
  || fail "the names weren't read as written"
for bad in 'tests::one' 'tests::one | soon' 'tests::one | #' ' | #1609'; do
  printf '%s\n' "$bad" > "$copy/.github/known-failures.txt"
  (cd "$copy" && .github/scripts/known-failures.sh names > /dev/null 2>&1) && fail "\`$bad\` was taken"
done
[ -n "$root" ] || fail "no repository"

if [ "$failures" -eq 0 ]; then
  echo "known-failures.sh: all passed"
fi
exit "$failures"
