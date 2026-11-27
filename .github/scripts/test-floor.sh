#!/usr/bin/env bash
# Tries `floor.sh` on floors of its own. A count at or over its floor passes and says so; one under
# it, zero, or one that isn't a number fails; a key with no floor fails apart, as a typo. A fuzz
# run's targets are each held to their own floor, a target with no line at all counting as 0, and
# the passed tests of every binary in a log are summed. `NIGHTLY_ONLY` lifts every floor, and each
# line goes to `$FLOORS_SEEN` and the step's summary.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() {
  echo "FAILED: $*"
  failures=$((failures + 1))
}
cat > "$work/floors.txt" << 'EOF'
# A comment, and a blank line, are passed over.

night/builds     100   builds    # 400, 420
fuzz-job/lexer   1000  inputs
fuzz-job/llvm    5     inputs
night/tests      3     tests
EOF
export FLOORS="$work/floors.txt"
unset NIGHTLY_ONLY GITHUB_STEP_SUMMARY FLOORS_SEEN
floor() {
  "$here/floor.sh" "$@" > "$work/said" 2>&1
}

floor night/builds 100 || fail "a count at its floor failed: $(cat "$work/said")"
grep -qx 'floor: night/builds did 100 builds (at least 100)' "$work/said" || fail "the line for a count that passed: $(cat "$work/said")"
floor night/builds 99
[ $? -eq 1 ] || fail "a count under its floor didn't exit 1"
grep -q 'under its floor of 100' "$work/said" || fail "a count under its floor didn't say so: $(cat "$work/said")"
grep -q '^::error::night/builds did 99 builds' "$work/said" || fail "a count under its floor gave no error"
floor night/builds 0
[ $? -eq 1 ] || fail "zero passed"
floor night/builds ""
[ $? -eq 1 ] || fail "no count passed"
floor night/builds 12x
[ $? -eq 1 ] || fail "a count that isn't a number passed"
grep -q "isn't a number" "$work/said" || fail "a count that isn't a number didn't say so"
floor night/bulids 400
[ $? -eq 2 ] || fail "a key with no floor didn't exit 2"
floor
[ $? -eq 2 ] || fail "no arguments didn't exit 2"

# Each target of a run is held to its own floor, and one that printed no line counts as 0.
cat > "$work/fuzz.txt" << 'EOF'
lexer: 2400000 inputs, 0 failures
lexer broke a rule: 1 of 2400000 inputs (7)
llvm: 4 inputs, 1 failures
EOF
floor --fuzz fuzz-job "$work/fuzz.txt"
[ $? -eq 1 ] || fail "a fuzz run with a target under its floor passed"
grep -qx 'floor: fuzz-job/lexer did 2400000 inputs (at least 1000)' "$work/said" || fail "the fuzz line for lexer: $(cat "$work/said")"
grep -qx 'floor: fuzz-job/llvm did 4 inputs, under its floor of 5' "$work/said" || fail "the fuzz line for llvm: $(cat "$work/said")"
echo 'llvm: 50 inputs, 0 failures' > "$work/fuzz.txt"
floor --fuzz fuzz-job "$work/fuzz.txt"
[ $? -eq 1 ] || fail "a target with no line passed"
grep -qx 'floor: fuzz-job/lexer did 0 inputs, under its floor of 1000' "$work/said" || fail "a target with no line didn't count as 0: $(cat "$work/said")"
floor --fuzz no-such-job "$work/fuzz.txt"
[ $? -eq 2 ] || fail "a prefix with no floors didn't exit 2"

# Every binary's passed tests are summed, libtest's and nextest's.
cat > "$work/tests.txt" << 'EOF'
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.1s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.0s
     Summary [   1.000s] 1 test run: 1 passed, 0 skipped
EOF
floor --tests night/tests "$work/tests.txt" || fail "3 tests passed didn't pass a floor of 3: $(cat "$work/said")"
grep -qx 'floor: night/tests did 3 tests (at least 3)' "$work/said" || fail "the tests line: $(cat "$work/said")"
echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.0s' > "$work/tests.txt"
floor --tests night/tests "$work/tests.txt"
[ $? -eq 1 ] || fail "a filter that matched no test passed"

# Lifted by hand, and each line kept where the report reads it.
NIGHTLY_ONLY=1 floor night/builds 3 || fail "NIGHTLY_ONLY didn't lift the floor"
FLOORS_SEEN="$work/seen" GITHUB_STEP_SUMMARY="$work/summary" floor night/builds 400
grep -qx 'floor: night/builds did 400 builds (at least 100)' "$work/seen" || fail "the line wasn't kept in FLOORS_SEEN"
grep -qx -- '- night/builds did 400 builds (at least 100)' "$work/summary" || fail "the line wasn't in the step's summary"

# The repository's own floors: each line a key, a number and a unit, and no key twice.
own="$here/../floors.txt"
bad=$(grep -vE '^\s*(#|$)' "$own" | awk '$2 !~ /^[0-9]+$/ || $3 == "" { print }')
[ -z "$bad" ] || fail "floors.txt lines without a number and a unit: $bad"
twice=$(grep -vE '^\s*(#|$)' "$own" | awk '{ print $1 }' | sort | uniq -d)
[ -z "$twice" ] || fail "floors.txt keys given twice: $twice"

# Every key the workflows and scripts name has a floor, and every prefix some: a typo there fails
# here, not on a night. A key made from a variable, as tsan-programs.sh's, is passed over.
named=$(grep -rhoE --exclude=test-floor.sh 'floor\.sh"? (--tests |--fuzz )?"?[a-z][A-Za-z0-9/_-]*' "$here/../workflows" "$here"/*.sh \
  | sed -E 's/.*floor\.sh"? (--tests |--fuzz )?"?//' | grep -v -- '-$' | sort -u)
for key in $named; do
  case "$key" in
    */*) grep -qE "^$key[[:space:]]" "$own" || fail "floors.txt has no floor for $key, which is named" ;;
    *) grep -qE "^$key([/[:space:]])" "$own" || fail "floors.txt has no floor for $key, which is named" ;;
  esac
done
[ -n "$named" ] || fail "no floor.sh call was found to check"

if [ "$failures" -ne 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "floor.sh: every case passed"
