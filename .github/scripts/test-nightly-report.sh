#!/usr/bin/env bash
# Tries `nightly-report.sh` against a GitHub that answers from files, with workflows made for it,
# so that a scheduled workflow that failed, stopped firing or was turned off can't pass unseen: each
# must fail the report. It needs `jq`, which reads the files as `gh --jq` reads GitHub's answers.
set -u
here="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d)"
trap '/bin/rm -rf "$work"' EXIT
failures=0

check() {
  if [ "$2" = "$3" ]; then
    echo "ok: $1"
  else
    echo "FAILED: $1: expected '$2', got '$3'"
    failures=$((failures + 1))
  fi
}

# The report and `main-ci.sh` beside workflows of their own: two nightly ones, one with its name
# quoted, a weekly one, and one only pushes start, which mustn't be read.
mkdir -p "$work/repo/.github/scripts" "$work/repo/.github/workflows" "$work/bin" "$work/gh" "$work/notes"
cp "$here/.github/scripts/nightly-report.sh" "$here/.github/scripts/main-ci.sh" "$work/repo/.github/scripts/"
workflow() {
  printf 'name: %s\n\non:\n%s  workflow_dispatch:\n' "$2" "$3" > "$work/repo/.github/workflows/$1"
}
workflow nightly.yml Nightly '  schedule:
    - cron: "1 2 * * *"
'
workflow weekly.yml '"Weekly, quoted"' '  schedule:
    - cron: "1 2 * * 3"
'
workflow proofs.yml Proofs '  schedule:
    - cron: "1 3 * * *"
'
workflow pushed.yml Pushed '  push:
    branches: [main]
'

# `gh` as the report calls it: each answer is a file under `$FAKE_GH`, run through `--jq`.
cat > "$work/bin/gh" << 'GH'
#!/usr/bin/env bash
words=("$@")
jq_expression="" workflow="" event=""
while [ $# -gt 0 ]; do
  case "$1" in
    --jq) jq_expression=$2; shift ;;
    --workflow) workflow=$2; shift ;;
    --event) event=$2; shift ;;
  esac
  shift
done
answer() {
  if [ -f "$1" ]; then cat "$1"; else echo "${2:-[]}"; fi | if [ -n "$jq_expression" ]; then jq -r "$jq_expression"; else cat; fi
}
path=${words[1]}
[ "$path" != --paginate ] || path=${words[2]}
case "${words[0]} $path" in
  "api rate_limit") echo '{}' ;;
  "api repos/"*"/branches"*) answer none ;;
  "api repos/"*"/actions/workflows/"*)
    [ -f "$FAKE_GH/workflow-${path##*/}.json" ] || exit 1
    answer "$FAKE_GH/workflow-${path##*/}.json" ;;
  "run list") answer "$FAKE_GH/runs-$workflow${event:+-$event}.json" ;;
  "run view") answer "$FAKE_GH/jobs-${words[2]}.json" '{"jobs": []}' ;;
  "run download") exit 1 ;;
  *) echo "fake gh: nothing to answer $* with" >&2; exit 1 ;;
esac
GH
chmod +x "$work/bin/gh"

# A run, as `<id> <status> <conclusion> <seconds ago>`, and the list of a workflow's runs.
run() {
  jq -nc --argjson id "$1" --arg status "$2" --arg conclusion "$3" --argjson ago "$4" \
    '{databaseId: $id, status: $status, conclusion: $conclusion, createdAt: (now - $ago | todate), headSha: "0123456789abc"}'
}
runs() {
  local file=$1
  shift
  (
    IFS=,
    echo "[$*]"
  ) > "$work/gh/runs-$file.json"
}
# A workflow as GitHub keeps it, made `<seconds ago>`, with its offset as GitHub writes it.
made() {
  jq -nc --arg state "$2" --argjson ago "$3" \
    '{state: $state, created_at: ((now - $ago | todate | .[0:19]) + ".000-05:00")}' > "$work/gh/workflow-$1.json"
}
day=86400
# Every workflow made a month ago, each scheduled one's newest run a success, and main's CI green.
green() {
  /bin/rm -f "$work"/gh/*.json
  for file in nightly.yml weekly.yml proofs.yml; do
    made "$file" active $((30 * day))
  done
  runs nightly.yml-schedule "$(run 10 completed success $((day / 2)))" "$(run 11 completed failure $((3 * day / 2)))"
  runs weekly.yml-schedule "$(run 20 completed success $((3 * day)))"
  runs proofs.yml-schedule "$(run 30 completed success $((day / 4)))"
  runs proofs.yml "$(run 30 completed success $((day / 4)))"
  runs CI "$(run 40 completed success 600)"
  runs "CI macOS" "$(run 41 completed success 600)"
  echo '{"jobs": [{"name": "Build", "conclusion": "failure", "steps": [{"name": "Compile", "conclusion": "failure"}]}]}' \
    > "$work/gh/jobs-12.json"
}
report() {
  PATH="$work/bin:$PATH" FAKE_GH="$work/gh" FENNEC_HOME="$work/home" \
    bash "$work/repo/.github/scripts/nightly-report.sh" "$work/notes" > "$work/out" 2>&1
  echo $?
}
said() {
  if grep -qF -- "$1" "$work/out"; then echo yes; else echo no; fi
}

green
check "every workflow green: the report passes" 0 "$(report)"
check "a nightly workflow is read by the name it gives" yes "$(said 'Nightly on ')"
check "a quoted name is read without its quotes" yes "$(said 'Weekly, quoted on ')"
check "Proofs' scheduled run is read" yes "$(said 'Proofs on ')"
check "a workflow with no schedule isn't read" no "$(said 'Pushed')"
[ "$(report)" = 0 ] || cat "$work/out"

green
runs nightly.yml-schedule "$(run 12 completed failure $((day / 2)))"
check "a scheduled run that failed fails the report" 1 "$(report)"
check "it names the job and the step that failed" yes "$(said 'failed: Build :: Compile')"

green
runs nightly.yml-schedule "$(run 13 completed cancelled $((day / 2)))"
check "a scheduled run that was cancelled fails the report" 1 "$(report)"

green
runs nightly.yml-schedule "$(run 14 in_progress "" 600)" "$(run 12 completed failure $((day + 600)))"
check "a failed run behind one still going fails the report" 1 "$(report)"
check "the one still going is said" yes "$(said 'Nightly: the scheduled run 14 is still going')"

green
runs weekly.yml-schedule "$(run 21 completed success $((10 * day)))"
check "a schedule that hasn't fired in 10 days fails the report" 1 "$(report)"
check "it says the schedule isn't firing" yes "$(said "Weekly, quoted: no scheduled run in 10 days, so its schedule isn't firing")"

green
made weekly.yml disabled_manually $((30 * day))
check "a scheduled workflow turned off fails the report" 1 "$(report)"
check "it says how it's turned off" yes "$(said 'its workflow is disabled_manually on GitHub')"

green
made weekly.yml active $((2 * day))
runs weekly.yml-schedule
check "a workflow two days old with no scheduled run yet passes" 0 "$(report)"
check "it says there's none yet" yes "$(said 'Weekly, quoted: no scheduled run yet')"

green
made weekly.yml active $((20 * day))
runs weekly.yml-schedule
check "a workflow 20 days old that never ran on its schedule fails the report" 1 "$(report)"

green
runs proofs.yml "$(run 31 in_progress "" 600)"
runs proofs.yml-schedule "$(run 32 completed failure $((day / 4)))"
check "Proofs' failed night behind a run by hand still going fails the report" 1 "$(report)"

green
runs CI "$(run 12 completed failure 600)"
check "main's CI red fails the report" 1 "$(report)"

if [ "$failures" -ne 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "every check passed"
