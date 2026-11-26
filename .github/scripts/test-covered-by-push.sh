#!/usr/bin/env bash
# Tries `covered-by-push.sh` against a GitHub answering from a file: a scheduled run is covered by
# a push run on its commit that's waiting, running or passed, and not by one that failed or was
# cancelled, or by none; any other event is never covered, and never asks.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() {
  echo "FAILED: $*"
  failures=$((failures + 1))
}

# A `gh run list` that answers from `$work/runs.json` through `jq -r` with what `--jq` asks, and
# notes what it was asked in `$work/asked`.
mkdir -p "$work/bin"
cat > "$work/bin/gh" << 'EOF'
#!/usr/bin/env bash
echo "$*" >> "$WORK/asked"
[ "$1" = run ] && [ "$2" = list ] || exit 2
shift 2
filter=.
while [ "$#" -gt 0 ]; do
  case "$1" in
    --jq) filter=$2; shift ;;
  esac
  shift
done
jq -r "$filter" "$WORK/runs.json"
EOF
chmod +x "$work/bin/gh"

# The push runs on the commit, each `status conclusion`.
runs() {
  local first=1 run
  {
    echo "["
    for run in "$@"; do
      read -r status conclusion <<< "$run"
      [ "$first" = 1 ] || echo ","
      first=0
      printf '{"databaseId":%s,"status":"%s","conclusion":"%s","url":"https://example/run/%s"}' \
        "$RANDOM" "$status" "$conclusion" "$status"
    done
    echo "]"
  } > "$work/runs.json"
  rm -f "$work/asked"
}
covered() {
  env PATH="$work/bin:$PATH" WORK="$work" EVENT="$1" SHA=abc123 GITHUB_REPOSITORY=owner/name \
    "$here/covered-by-push.sh" 2> "$work/said"
}

runs "in_progress "
[ "$(covered schedule)" = true ] || fail "a push run in progress didn't cover it: $(cat "$work/said")"
grep -q 'ci.yml --event push --commit abc123' "$work/asked" || fail "it didn't ask about this commit's push runs: $(cat "$work/asked")"
grep -q 'is in_progress' "$work/said" || fail "it didn't say why: $(cat "$work/said")"

runs "queued "
[ "$(covered schedule)" = true ] || fail "a waiting push run didn't cover it"

runs "completed success"
[ "$(covered schedule)" = true ] || fail "a push run that passed didn't cover it"

runs "completed failure"
[ "$(covered schedule)" = false ] || fail "a push run that failed covered it"

runs "completed cancelled"
[ "$(covered schedule)" = false ] || fail "a cancelled push run covered it"

runs "completed failure" "in_progress "
[ "$(covered schedule)" = true ] || fail "a push run in progress beside a failed one didn't cover it"

runs
[ "$(covered schedule)" = false ] || fail "no push run covered it"
grep -q 'runs every job' "$work/said" || fail "it didn't say why not: $(cat "$work/said")"

runs "completed success"
[ "$(covered push)" = false ] || fail "a push was covered"
[ "$(covered workflow_dispatch)" = false ] || fail "a run by hand was covered"
[ -f "$work/asked" ] && fail "it asked GitHub for an event that's never covered"

if [ "$failures" -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "covered-by-push.sh: every case as it should be"
