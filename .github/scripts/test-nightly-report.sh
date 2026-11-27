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
cp "$here/.github/scripts/nightly-report.sh" "$here/.github/scripts/main-ci.sh" "$here/.github/scripts/mutants-verdict.sh" \
  "$work/repo/.github/scripts/"
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
jq_expression="" workflow="" event="" dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --jq) jq_expression=$2; shift ;;
    --dir) dir=$2; shift ;;
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
  "api repos/"*"/commits"*) answer "$FAKE_GH/commits.json" ;;
  "api repos/"*"/actions/workflows/"*)
    [ -f "$FAKE_GH/workflow-${path##*/}.json" ] || exit 1
    answer "$FAKE_GH/workflow-${path##*/}.json" ;;
  "run list") answer "$FAKE_GH/runs-$workflow${event:+-$event}.json" ;;
  "run view")
    # A run's log, a line `<job>\t<step>\t<time> <what it printed>`, or its jobs.
    if [[ " ${words[*]} " == *" --log "* ]]; then
      # A folder in the file's place stands for a log GitHub wouldn't give.
      [ -d "$FAKE_GH/log-${words[2]}.txt" ] && exit 1
      [ ! -f "$FAKE_GH/log-${words[2]}.txt" ] || cat "$FAKE_GH/log-${words[2]}.txt"
    else
      answer "$FAKE_GH/jobs-${words[2]}.json" '{"jobs": []}'
    fi ;;
  "run download")
    [ -d "$FAKE_GH/download-${words[2]}" ] || exit 1
    mkdir -p "$dir" && cp -R "$FAKE_GH/download-${words[2]}/." "$dir/" ;;
  *) echo "fake gh: nothing to answer $* with" >&2; exit 1 ;;
esac
GH
chmod +x "$work/bin/gh"

# A run, as `<id> <status> <conclusion> <seconds ago> [<commit>]`, and the list of a workflow's
# runs.
run() {
  jq -nc --argjson id "$1" --arg status "$2" --arg conclusion "$3" --argjson ago "$4" --arg sha "${5:-0123456789abc}" \
    '{databaseId: $id, status: $status, conclusion: $conclusion, createdAt: (now - $ago | todate), headSha: $sha}'
}
# Main's commits, newest first, as GitHub lists them.
commits() {
  printf '%s\n' "$@" | jq -Rn '[inputs | {sha: .}]' > "$work/gh/commits.json"
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
  /bin/rm -f "$work"/gh/*.json "$work"/gh/*.txt
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

# Each step's count beside its floor, as `floor.sh` printed it in the run's log, under the run.
green
printf 'Fuzz (checked)\tFuzz every target\t2026-11-27T05:00:00.0000000Z floor: fuzz-checked/lexer did 2400000 inputs (at least 600000)\n' > "$work/gh/log-10.txt"
printf 'Fuzz (checked)\tFuzz every target\t2026-11-27T05:00:00.0000000Z floor: fuzz-checked/parser did 0 inputs, under its floor of 450000\n' >> "$work/gh/log-10.txt"
check "a run's floors don't change how the report ends" 0 "$(report)"
check "a step's count is said beside its floor, by its job" yes "$(said '  Fuzz (checked): fuzz-checked/lexer did 2400000 inputs (at least 600000)')"
check "a step under its floor is said so" yes "$(said '  Fuzz (checked): fuzz-checked/parser did 0 inputs, under its floor of 450000')"
# Two jobs saying the same count of one key are each said, not merged.
green
printf 'Miri (shard 1 of 8)\tRun\t2026-11-27T05:00:00.0000000Z floor: miri/passed did 50 tests (at least 11)\n' > "$work/gh/log-10.txt"
printf 'Miri (shard 2 of 8)\tRun\t2026-11-27T05:00:00.0000000Z floor: miri/passed did 50 tests (at least 11)\n' >> "$work/gh/log-10.txt"
report > /dev/null
check "the first of two shards saying the same is said" yes "$(said '  Miri (shard 1 of 8): miri/passed did 50 tests')"
check "the second of two shards saying the same is said" yes "$(said '  Miri (shard 2 of 8): miri/passed did 50 tests')"
# A workflow whose steps are held to floors, whose run said none, is said so.
echo '# .github/scripts/floor.sh --fuzz fuzz-checked fuzz.txt' >> "$work/repo/.github/workflows/weekly.yml"
report > /dev/null
check "a run of a workflow with floors that said none is said so" yes "$(said '  no step said how much it did')"
# A log GitHub wouldn't give is said so, not read as no counts.
mkdir -p "$work/gh/log-20.txt"
report > /dev/null
check "a log that couldn't be read is said so" yes "$(said "  its log couldn't be read")"
/bin/rm -rf "$work/gh/log-20.txt"
sed -i.bak '$d' "$work/repo/.github/workflows/weekly.yml" && /bin/rm -f "$work/repo/.github/workflows/weekly.yml.bak"

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

# Run 12's job failed already, in a run still going: that's red now, not when the run ends.
green
runs CI "$(run 12 in_progress "" 300)" "$(run 40 completed success 600)"
check "a job failed in main's CI run still going fails the report" 1 "$(report)"
check "it names the run still going and its job" yes "$(said 'main CI: red at 012345678 (run 12, still going): Build since 012345678')"

green
runs CI "$(run 42 in_progress "" 300)" "$(run 40 completed success 600)"
check "main's CI run still going with no job failed yet leaves it green" 0 "$(report)"
check "it goes by the run that finished" yes "$(said 'main CI: green at 012345678 (run 40)')"

# A run going that's older than one that finished says nothing new.
green
runs CI "$(run 40 completed success 300)" "$(run 12 in_progress "" 600)"
check "a run still going behind one that finished isn't read" 0 "$(report)"

# A run of a commit made after its child's run: the child's is the newer, by main's history. On
# 2026-11-21 the parent's red run, made 4 minutes after the fix's green one, was taken as main's.
green
commits bbbbbbbbbbbb aaaaaaaaaaaa
echo '{"jobs": [{"name": "Test", "conclusion": "failure"}]}' > "$work/gh/jobs-51.json"
runs CI "$(run 51 completed failure 300 aaaaaaaaaaaa)" "$(run 50 completed success 540 bbbbbbbbbbbb)"
check "a commit's run made after its child's doesn't count as newer" 0 "$(report)"
check "the child's run counts" yes "$(said 'main CI: green at bbbbbbbbb (run 50)')"
# The same for a run still going: one of the parent, made later, with a job failed, isn't read.
runs CI "$(run 51 in_progress "" 300 aaaaaaaaaaaa)" "$(run 50 completed success 540 bbbbbbbbbbbb)"
check "a run still going of an older commit isn't read" 0 "$(report)"
check "the newer commit's finished run counts" yes "$(said 'main CI: green at bbbbbbbbb (run 50)')"
# And the other way: the child's red, made first, counts, with the parent's green run behind it.
/bin/rm -f "$work/gh/jobs-51.json"
echo '{"jobs": [{"name": "Test", "conclusion": "failure"}]}' > "$work/gh/jobs-50.json"
runs CI "$(run 51 completed success 300 aaaaaaaaaaaa)" "$(run 50 completed failure 540 bbbbbbbbbbbb)"
check "the child's red run made first counts" 1 "$(report)"
check "it's red since the child" yes "$(said 'main CI: red at bbbbbbbbb (run 50): Test since bbbbbbbbb')"

green
check "with no night of mutants yet, the report says so" yes "$(said 'mutants: no nightly run yet')"

# A night's mutants, each crate's lists as cargo-mutants writes them, `<list>=<count>`.
slice() {
  local folder=$work/gh/download-50/mutants-nightly-$1
  shift
  mkdir -p "$folder/mutants.out"
  echo "## ${folder##*-nightly-}, slice 14/60" > "$folder/ranked.md"
  for list in "$@"; do
    : > "$folder/mutants.out/${list%=*}.txt"
    local i=0
    while [ "$i" -lt "${list#*=}" ]; do
      i=$((i + 1))
      echo "crates/ir/src/lower.rs:$i:9: replace && with ||" >> "$folder/mutants.out/${list%=*}.txt"
    done
  done
}
green
runs mutants.yml-schedule "$(run 50 completed success $((day / 2)))"
slice fennec-ir caught=3 missed=2 timeout=1 unviable=0
slice fennec-llvm caught=7 missed=0 timeout=0 unviable=2
check "a night of mutants passes the report" 0 "$(report)"
check "it says one crate's counts" yes "$(said "fennec-ir: 3 caught, 2 missed, 0 equivalent, 1 timed out, 0 didn't build")"
check "and another's" yes "$(said "fennec-llvm: 7 caught, 0 missed, 0 equivalent, 0 timed out, 2 didn't build")"
check "and what no test caught, all told" yes "$(said ": 2 no test caught, ranked by file")"

if [ "$failures" -ne 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "every check passed"
