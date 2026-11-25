#!/usr/bin/env bash
# Tries `ci-passed.sh` against a GitHub answering from files: a job GitHub never gave a runner is
# said so plainly and still fails a push, only warns for a macOS job on a pull request, is found
# outside the matrices too, and isn't confused with a job cancelled because its run was.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() {
  echo "FAILED: $*"
  failures=$((failures + 1))
}

# A `gh api` that answers from `$work/answers`: the run's jobs, and each check's notes, run through
# `jq -r` with what `--jq` asks, as gh does.
mkdir -p "$work/bin"
cat > "$work/bin/gh" << 'EOF'
#!/usr/bin/env bash
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
  rm -rf "$work/answers"
  mkdir -p "$work/answers"
  local IFS=,
  echo "{\"jobs\":[$*]}" > "$work/answers/jobs.json"
}
not_acquired() {
  echo '[{"message":"The job was not acquired by Runner of type hosted even after multiple attempts"}]' \
    > "$work/answers/notes-$1.json"
}
# Runs it, keeping what it says and what it sums up, and gives its status.
decide() {
  local event=$1 needs=$2
  : > "$work/summary"
  env PATH="$work/bin:$PATH" ANSWERS="$work/answers" NEEDS="$needs" EVENT="$event" PLATFORMS=all \
    RUN_ID=7 RUN_ATTEMPT=1 GITHUB_REPOSITORY=owner/name GITHUB_STEP_SUMMARY="$work/summary" \
    "$here/ci-passed.sh" > "$work/said" 2>&1
}

mac=$(job 1 "Test (macos-latest, fennec)" cancelled "" 0)
linux=$(job 2 "Test (ubuntu-latest, fennec)" success "runner 1" 3)
intel=$(job 3 "Native code on x86-64 macOS (llvm::a)" success "runner 2" 3)
needs='{"changes":{"result":"success"},"test":{"result":"failure"},"x86-mac":{"result":"success"}}'

# A macOS job GitHub never gave a runner, on a push: red, and said so plainly.
answers "$mac" "$linux" "$intel"
not_acquired 1
decide push "$needs" && fail "a push with a job that never got a runner passed"
grep -q '::error title=No runner::Test (macos-latest, fennec) never got a runner: GitHub had none to give it, so nothing in the code failed' "$work/said" \
  || fail "it didn't say plainly the job never got a runner: $(cat "$work/said")"
grep -q 'gh run rerun 7 --failed' "$work/said" || fail "it didn't say how to run it again: $(cat "$work/said")"
grep -q '^### Never got a runner' "$work/summary" || fail "the summary doesn't list it apart: $(cat "$work/summary")"
grep -q '^### Failed' "$work/summary" && fail "the summary lists it as a failure: $(cat "$work/summary")"

# The same on a pull request only warns.
decide pull_request "$needs" || fail "a pull request's macOS job that never got a runner failed it: $(cat "$work/said")"
grep -q '::warning::Test (macos-latest, fennec) never got a runner' "$work/said" || fail "it didn't warn: $(cat "$work/said")"

# A job cancelled with no runner and no steps, but whose check says nothing of a runner, as when
# the run was cancelled, is a cancellation, not a want of runners.
answers "$mac" "$linux" "$intel"
decide push "$needs" && fail "a cancelled job passed"
grep -q 'Test (macos-latest, fennec) finished with: cancelled' "$work/said" || fail "it didn't say the job was cancelled: $(cat "$work/said")"
grep -q 'never got a runner' "$work/said" && fail "it took a cancelled job for one with no runner: $(cat "$work/said")"

# Outside the matrices, a job is found by its name, which `NEEDS` doesn't give.
bench=$(job 4 "Benchmarks" cancelled "" 0)
answers "$linux" "$intel" "$(job 1 "Test (macos-latest, fennec)" success "runner 3" 3)" "$bench"
not_acquired 4
decide push '{"changes":{"result":"success"},"test":{"result":"success"},"x86-mac":{"result":"success"},"bench":{"result":"cancelled"}}' \
  && fail "a push with a job outside the matrices that never got a runner passed"
grep -q '::error title=No runner::Benchmarks never got a runner' "$work/said" || fail "it didn't name the job outside the matrices: $(cat "$work/said")"

# A job that ran and failed is a failure, with no word of runners.
answers "$(job 1 "Test (macos-latest, fennec)" failure "runner 3" 9)" "$linux" "$intel"
decide pull_request "$needs" && fail "a macOS job that ran and failed passed a pull request"
grep -q 'Test (macos-latest, fennec) finished with: failure' "$work/said" || fail "it didn't say what failed: $(cat "$work/said")"
grep -q 'never got a runner' "$work/said" && fail "it spoke of runners for a failure: $(cat "$work/said")"

# Every job passing passes.
answers "$(job 1 "Test (macos-latest, fennec)" success "runner 3" 9)" "$linux" "$intel"
decide push '{"changes":{"result":"success"},"test":{"result":"success"},"x86-mac":{"result":"success"}}' \
  || fail "every job passing didn't pass: $(cat "$work/said")"

if [ "$failures" -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "ci-passed.sh: every case as it should be"
