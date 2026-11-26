#!/usr/bin/env bash
# Tries `release-passed.sh` against a GitHub answering from a file: the nightly run is skipped
# when a run of the release workflow on its commit passed, other than a pull request's, and not
# otherwise; any other event never skips, and never asks.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() {
  echo "FAILED: $*"
  failures=$((failures + 1))
}

# A `gh run list` that answers from `$work/runs.json`, as gh does: only runs with the status
# asked for, through `jq -r` with what `--jq` asks. It notes what it was asked in `$work/asked`.
mkdir -p "$work/bin"
cat > "$work/bin/gh" << 'EOF'
#!/usr/bin/env bash
echo "$*" >> "$WORK/asked"
[ "$1" = run ] && [ "$2" = list ] || exit 2
shift 2
filter=. status=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --jq) filter=$2; shift ;;
    --status) status=$2; shift ;;
  esac
  shift
done
jq -r --arg status "$status" "[.[] | select(\$status == \"\" or .conclusion == \$status)] | $filter" "$WORK/runs.json"
EOF
chmod +x "$work/bin/gh"

# The release runs on the commit, each `event conclusion`.
runs() {
  local first=1 run n=0
  {
    echo "["
    for run in "$@"; do
      read -r event conclusion <<< "$run"
      n=$((n + 1))
      [ "$first" = 1 ] || echo ","
      first=0
      printf '{"databaseId":%s,"event":"%s","conclusion":"%s","url":"https://example/run/%s"}' "$n" "$event" "$conclusion" "$n"
    done
    echo "]"
  } > "$work/runs.json"
  rm -f "$work/asked"
}
passed() {
  env PATH="$work/bin:$PATH" WORK="$work" EVENT="$1" SHA=abc123 GITHUB_REPOSITORY=owner/name \
    "$here/release-passed.sh" 2> "$work/said"
}

runs "schedule success"
[ "$(passed schedule)" = true ] || fail "last night's pass on the same commit didn't skip tonight: $(cat "$work/said")"
grep -q 'release.yml --commit abc123 --status success' "$work/asked" || fail "it didn't ask for this commit's passes: $(cat "$work/asked")"
grep -q 'nothing to add' "$work/said" || fail "it didn't say why: $(cat "$work/said")"

runs "workflow_dispatch success"
[ "$(passed schedule)" = true ] || fail "a run by hand that passed didn't skip tonight"

runs "pull_request success"
[ "$(passed schedule)" = false ] || fail "a pull request's pass, which built another commit, skipped tonight"

runs "schedule failure"
[ "$(passed schedule)" = false ] || fail "a failed run skipped tonight"

runs
[ "$(passed schedule)" = false ] || fail "no run skipped tonight"
grep -q 'builds it' "$work/said" || fail "it didn't say why not: $(cat "$work/said")"

runs "schedule success"
[ "$(passed workflow_dispatch)" = false ] || fail "a run by hand was skipped"
[ "$(passed push)" = false ] || fail "a tag was skipped"
[ "$(passed pull_request)" = false ] || fail "a pull request was skipped"
[ -f "$work/asked" ] && fail "it asked GitHub for an event that's never skipped"

if [ "$failures" -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "release-passed.sh: every case as it should be"
