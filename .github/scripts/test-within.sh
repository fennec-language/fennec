#!/usr/bin/env bash
# Tries `within.sh` on a command that finishes, one that runs too long, and one that won't stop
# when it's killed, as an x86-64 program won't while Rosetta is wedged: within.sh has to give up
# on that one a few seconds after stopping it, rather than wait for it forever.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
stubborn=""
trap '[ -n "$stubborn" ] && kill -KILL "$stubborn" 2> /dev/null; rm -rf "$work"' EXIT
failures=0
fail() {
  echo "FAILED: $*"
  failures=$((failures + 1))
}

# A command that finishes in time gives its own status.
"$here/within.sh" 10 bash -c 'exit 3' 2> /dev/null
[ $? -eq 3 ] || fail "a command that finished didn't give its own status"

# One that runs too long is stopped, with everything it started, and gives 124.
started=$SECONDS
# The command's own shell expands what's quoted.
# shellcheck disable=SC2016
"$here/within.sh" 1 bash -c 'sleep 30 & echo $! > "$0/child"; wait' "$work" 2> "$work/said"
status=$?
[ $status -eq 124 ] || fail "a command that ran too long gave $status, not 124"
[ $((SECONDS - started)) -lt 10 ] || fail "a command that ran too long took $((SECONDS - started))s to stop"
grep -q "didn't finish within 1 seconds" "$work/said" || fail "it didn't say what ran too long: $(cat "$work/said")"
sleep 0.5
! kill -0 "$(cat "$work/child")" 2> /dev/null || fail "what the command started was left running"

# One that won't stop. Nothing ignores the KILL within.sh sends, so it's told to send TERM, which
# this command ignores, as a process the system can't kill ignores everything.
started=$SECONDS
# The command's own shell expands what's quoted.
# shellcheck disable=SC2016
WITHIN_SIGNAL=TERM "$here/within.sh" 1 bash -c 'trap "" TERM; echo $$ > "$0/stubborn"; while :; do sleep 0.2; done' "$work" 2> "$work/said"
status=$?
stubborn="$(cat "$work/stubborn" 2> /dev/null)"
[ $status -eq 124 ] || fail "a command that wouldn't stop gave $status, not 124"
[ $((SECONDS - started)) -lt 15 ] || fail "within.sh waited $((SECONDS - started))s for a command that wouldn't stop"
grep -q "left running as process $stubborn" "$work/said" || fail "it didn't say what was left running: $(cat "$work/said")"
kill -0 "$stubborn" 2> /dev/null || fail "the command that wouldn't stop stopped, so this tried nothing"

if [ $failures -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "within.sh: all passed"
