#!/usr/bin/env bash
# Tries `rerun-no-runner.sh` against a GitHub answering from files: a run whose only failures are
# jobs that never got a runner runs again, but not a third time, and never one with any other
# failure, or with a job cancelled for another reason.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() {
  echo "FAILED: $*"
  failures=$((failures + 1))
}

# A `gh` that answers `api` from `$work/answers`, through `jq -r` with what `--jq` asks, and notes
# each `run rerun` in `$work/reran`.
mkdir -p "$work/bin"
cat > "$work/bin/gh" << 'EOF'
#!/usr/bin/env bash
if [ "$1" = run ] && [ "$2" = rerun ]; then
  echo "$*" >> "$ANSWERS/../reran"
  exit 0
fi
[ "$1" = api ] || exit 2
shift
filter=. endpoint=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --paginate) ;;
    --jq) filter=$2; shift ;;
    *) endpoint=$1 ;;
  esac
  shift
done
case "$endpoint" in
  */jobs) file=jobs.json ;;
  */check-runs/*/annotations) id=${endpoint#*/check-runs/}; file="notes-${id%%/*}.json" ;;
  *) exit 2 ;;
esac
[ -f "$ANSWERS/$file" ] || { echo '[]' | jq -r "$filter"; exit 0; }
jq -r "$filter" "$ANSWERS/$file"
EOF
chmod +x "$work/bin/gh"

# A job as the API gives one: its id, name, conclusion, runner and how many steps it has.
job() {
  local steps='[{"name":"Set up job"}]'
  [ "$5" = 0 ] && steps='[]'
  local runner='null'
  [ -n "$4" ] && runner="\"$4\""
  printf '{"id":%s,"name":"%s","conclusion":"%s","runner_name":%s,"steps":%s}' "$1" "$2" "$3" "$runner" "$steps"
}
answers() {
  rm -rf "$work/answers" "$work/reran"
  mkdir -p "$work/answers"
  local IFS=,
  echo "{\"jobs\":[$*]}" > "$work/answers/jobs.json"
}
not_acquired() {
  echo '[{"message":"The job was not acquired by Runner of type hosted even after multiple attempts"}]' \
    > "$work/answers/notes-$1.json"
}
# Runs it on attempt `$1` of run 7, keeping what it says.
rerun() {
  env PATH="$work/bin:$PATH" ANSWERS="$work/answers" RUN_ID=7 RUN_ATTEMPT="$1" GITHUB_REPOSITORY=owner/name \
    "$here/rerun-no-runner.sh" > "$work/said" 2>&1
}

mac=$(job 1 "ci / Test (macos-latest, fennec)" cancelled "" 0)
intel=$(job 2 "ci / Native code on x86-64 macOS (llvm::a)" success "runner 1" 5)
summary=$(job 3 "ci / CI passed" failure "runner 2" 4)

# Only a job that never got a runner failed, and the summary job with it: it runs again.
answers "$mac" "$intel" "$summary"
not_acquired 1
rerun 1 || fail "it failed: $(cat "$work/said")"
grep -qx 'run rerun 7 --failed --repo owner/name' "$work/reran" 2> /dev/null || fail "it didn't run the failed jobs again: $(cat "$work/said")"
grep -q 'ci / Test (macos-latest, fennec)' "$work/said" || fail "it didn't say which job: $(cat "$work/said")"

# The same on its second attempt runs again; on its third it doesn't.
answers "$mac" "$intel" "$summary"
not_acquired 1
rerun 2
[ -f "$work/reran" ] || fail "a second attempt didn't run again"
answers "$mac" "$intel" "$summary"
not_acquired 1
rerun 3
[ -f "$work/reran" ] && fail "a third attempt ran again"
grep -q 'has run 3 times' "$work/said" || fail "it didn't say why it stopped: $(cat "$work/said")"

# Any other failure beside it, and nothing runs again.
answers "$mac" "$(job 2 "ci / Native code on x86-64 macOS (llvm::a)" failure "runner 1" 5)" "$summary"
not_acquired 1
rerun 1
[ -f "$work/reran" ] && fail "a run with a real failure ran again"
grep -q 'Native code on x86-64 macOS (llvm::a): failure' "$work/said" || fail "it didn't name the real failure: $(cat "$work/said")"

# A job cancelled with no runner, but with no note of one, isn't a want of runners.
answers "$mac" "$intel" "$summary"
rerun 1
[ -f "$work/reran" ] && fail "a job cancelled for another reason ran again"

if [ "$failures" -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "rerun-no-runner.sh: every case as it should be"
