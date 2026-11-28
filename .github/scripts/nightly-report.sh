#!/usr/bin/env bash
# Copies the latest nightly Proofs run's report into a folder, as `nightly-proofs-<date>.md`, and
# says in one line how the night went:
#
#   .github/scripts/nightly-report.sh "$HOME/Library/Application Support/Claude/side-session-notes/<lanes>"
#
# A second argument names the repository, `fennec-language/fennec` unless given.
#
# It also copies the night's mutants no test caught, ranked by file, as `mutants-<date>.md`, and
# this Mac's flake ledger (see `flakes.sh`), listed by test with `fennec-flakes` and
# each entry whole under it, as `flakes-<date>.md`, so every flake the gates saw is there to read.
#
# It says too whether main's CI is green on every system, `CI` and `CI macOS` each in a line,
# naming each job that isn't, with `main-ci.sh`, and warns when GitHub holds more than 25 branches.
#
# It says how the newest scheduled run of every workflow with a schedule ended too, each found in
# `.github/workflows`: Fuzzing, Release, Mutation tests, the weekly big codebase, CodeQL and the
# rest, and CI's own run every four hours. Under each run that failed, it names each job that
# failed and the step it failed at, a line each, as `failed: <job> :: <step>`, and for Proofs those
# lines head the copied report, so a night that failed in no test, as a soak can, still says what
# failed.
#
# It needs `gh`, signed in. A night still running, or one with no report, is said so, and nothing
# is written. It exits 1 when any scheduled workflow's newest finished run didn't succeed, when one
# is turned off or its schedule hasn't fired in 8 days, when a job of main's CI is red on any
# system, or when GitHub doesn't answer, so whatever runs it can say so loudly.
set -eu
folder=$1
here=$(cd "$(dirname "$0")" && pwd)
ledger="${FENNEC_HOME:-$HOME/.fennec}/flakes.log"
today=$(date -u +%Y-%m-%d)
if [ -f "$ledger" ]; then
  {
    echo "# Flakes the gates on this Mac saw, $today"
    echo
    echo '```'
    "$here/fennec-flakes" "$ledger"
    echo '```'
    echo
    echo "## Every entry"
    echo
    echo '```'
    cat "$ledger"
    echo '```'
  } > "$folder/flakes-$today.md"
  echo "flakes: $(grep -c '^=== flake$' "$ledger") in the ledger, listed in $folder/flakes-$today.md"
else
  echo "flakes: none, as there's no ledger at $ledger"
fi
# Failures that passed once main changed what they test, kept out of the ledger (see `flakes.sh`),
# counted so one that was a flake after all can still be found.
fixed_on_main="${FENNEC_HOME:-$HOME/.fennec}/fixed-on-main.log"
if [ -f "$fixed_on_main" ]; then
  echo "fixed on main: $(grep -c '^=== fixed on main$' "$fixed_on_main") failures that passed after main changed what they test, in $fixed_on_main"
fi
repository=${2:-fennec-language/fennec}
# With GitHub not answering, every run below would read as none yet, so it's said once, plainly.
if ! gh api rate_limit > /dev/null 2>&1; then
  echo "GitHub isn't answering, so the night can't be read: try again in a while"
  exit 1
fi
# Main's CI on every system, a line for `CI` and one for `CI macOS`: a job red on one, like
# Windows, is said here, and fails the report, the day it goes red.
main_ci=0
"$here/main-ci.sh" "$repository" || main_ci=1
# The weekly sweep (`sweep-branches.yml`) deletes the branches nothing needs, so past 25 something
# is keeping them, or it isn't running: a warning, not a failed night.
branches=$(gh api --paginate "repos/$repository/branches?per_page=100" --jq 'length' 2> /dev/null \
  | awk '{ total += $1 } END { print total + 0 }')
if [ "$branches" -gt 25 ]; then
  echo "warning: $repository has $branches branches, more than 25: https://github.com/$repository/branches"
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# A run's id, how it ended, and when it started, on a line. A run still going has a conclusion of
# "" rather than none, which `//` passes over, so it's said as `running` here: left empty, `read`
# took the start for the conclusion, and a night still going read as one that kept no report.
how_ended='.[0] // empty | "\(.databaseId) \(if (.conclusion // "") == "" then "running" else .conclusion end) \(.createdAt)"'
# Each job of a run that didn't succeed, and the step it failed at, a line each, as in
#
#   failed: Soak web_backend for an hour, in main :: Run it for an hour, reading what it holds every minute
#
# A job that ran out of time or was cancelled can have no step that failed: it's said so.
failed_steps() {
  gh run view "$1" --repo "$repository" --json jobs --jq '.jobs[]
    | select(.conclusion == "failure" or .conclusion == "timed_out" or .conclusion == "cancelled")
    | .conclusion as $how
    | [.steps[] | select(.conclusion == "failure" or .conclusion == "timed_out" or .conclusion == "cancelled") | .name] as $steps
    | "  failed: \(.name) :: \(if ($steps | length) == 0 then "no step failed, the job ended \($how)" else $steps | join(", ") end)"'
}
# How much each step of run `$1` did, beside its floor, a line each as `floor.sh` said it in the
# run's log, named by its job, so "ran 0 inputs" can't look green and the shards and soaks that say
# the same key can be told apart. A workflow `$2` whose steps are held to floors but whose run said
# none is said so: its steps didn't get as far as counting. A log that couldn't be read is said so
# apart, as it says nothing of the steps.
floors_of() {
  local log said
  if ! log=$(gh run view "$1" --repo "$repository" --log 2> /dev/null); then
    grep -q 'floor\.sh' "$here/../workflows/$2" 2> /dev/null && echo "  its log couldn't be read, so how much each step did isn't known"
    return 0
  fi
  said=$(awk -F '\t' 'index($0, "floor: ") { line = $0; sub(/^.*floor: /, "", line); print "  " $1 ": " line }' <<< "$log" \
    | awk '!seen[$0]++')
  if [ -n "$said" ]; then
    echo "$said"
  elif grep -q 'floor\.sh' "$here/../workflows/$2" 2> /dev/null; then
    echo "  no step said how much it did"
  fi
}
# Whether a scheduled workflow's night failed, which fails the report as main's CI does.
nights=0
# How the newest finished scheduled run of the workflow `file`, called `name`, ended, in a line:
# anything but success fails the night, a cancelled run or one out of time too, and under it each
# job that failed and the step it failed at. A run still going is said, and the one before it is
# still read, so a failed night can't hide behind the next one. A workflow that's turned off, or
# whose schedule hasn't fired in 8 days, a week and a day, fails the night too, as nothing it would
# catch is caught. Release's nightly was red for days unseen until it was read here.
nightly() {
  local name=$1 file=$2 state age runs newest going run conclusion started
  read -r state age <<< "$(gh api "repos/$repository/actions/workflows/$file" \
    --jq '"\(.state) \(now - (.created_at[0:19] + "Z" | fromdateiso8601) | floor)"')"
  if [ "$state" != active ]; then
    echo "$name: its workflow is ${state:-unknown} on GitHub, so it doesn't run on its schedule"
    nights=1
    return 0
  fi
  # Each run a line, newest first, as `<id> <status> <conclusion> <started> <seconds since>`.
  if ! runs=$(gh run list --repo "$repository" --workflow "$file" --event schedule --limit 5 \
    --json databaseId,status,conclusion,createdAt \
    --jq '.[] | "\(.databaseId) \(.status) \(if (.conclusion // "") == "" then "none" else .conclusion end) \(.createdAt) \(now - (.createdAt | fromdateiso8601) | floor)"'); then
    echo "$name: its scheduled runs couldn't be read"
    nights=1
    return 0
  fi
  newest=$(head -n 1 <<< "$runs" | cut -d ' ' -f 5)
  if [ -z "$newest" ] && [ "${age:-0}" -le $((8 * 86400)) ]; then
    echo "$name: no scheduled run yet"
    return 0
  fi
  if [ "${newest:-$age}" -gt $((8 * 86400)) ]; then
    echo "$name: no scheduled run in $((${newest:-$age} / 86400)) days, so its schedule isn't firing"
    nights=1
  fi
  going=$(awk '$2 != "completed" { print $1; exit }' <<< "$runs")
  [ -z "$going" ] || echo "$name: the scheduled run $going is still going"
  read -r run _ conclusion started _ <<< "$(awk '$2 == "completed" { print; exit }' <<< "$runs")"
  [ -n "${run:-}" ] || return 0
  echo "$name on ${started%%T*}: $conclusion, https://github.com/$repository/actions/runs/$run"
  floors_of "$run" "$file"
  if [ "$conclusion" != success ]; then
    nights=1
    failed_steps "$run" || echo "  failed: its jobs couldn't be read"
  fi
}
# Every workflow that runs on a schedule, found in the workflows beside this script, so one added
# later is read too, by the name GitHub shows. Proofs is read here as well as below, where its
# report is copied from its newest run, which a run by hand still going hid the night behind. CI
# is read here as well as by `main-ci.sh`, which reads main's newest run of any kind: the heap
# audit and coverage run only on the schedule, so a push's run after a red one hid it.
for file in "$here"/../workflows/*.yml; do
  grep -qE '^  schedule:' "$file" || continue
  name=$(sed -n 's/^name: *//p' "$file" | head -n 1)
  name=${name#[\"\']}
  name=${name%[\"\']}
  nightly "${name:-${file##*/}}" "${file##*/}"
done
# The night's mutants, from the latest scheduled Mutation tests run, as `mutants-<date>.md`: each
# crate's slice, with what no test caught ranked by file, and a line a crate here with how many
# mutants were caught, missed and timed out. Survivors are reported, never a failed night. A slice
# where most timed out fails its job, which fails the report above.
read -r mutants finished started <<< "$(gh run list --repo "$repository" --workflow mutants.yml --event schedule \
  --limit 1 --json databaseId,status,createdAt --jq '.[0] // empty | "\(.databaseId) \(.status) \(.createdAt)"')"
if [ -z "${mutants:-}" ]; then
  echo "mutants: no nightly run yet"
elif [ "$finished" != completed ]; then
  echo "mutants: the nightly run $mutants is still going"
elif gh run download "$mutants" --repo "$repository" --pattern 'mutants-nightly-*' --dir "$work/mutants" > /dev/null 2>&1; then
  night=${started%%T*}
  {
    echo "# Mutants no test caught, $night"
    echo
    echo "https://github.com/$repository/actions/runs/$mutants"
    # One artifact comes out in the folder itself, several in a folder each.
    for ranked in "$work"/mutants/ranked.md "$work"/mutants/*/ranked.md; do
      [ -f "$ranked" ] || continue
      echo
      cat "$ranked"
    done
  } > "$folder/mutants-$night.md"
  for out in "$work"/mutants/mutants.out "$work"/mutants/*/mutants.out; do
    [ -d "$out" ] || continue
    crate=$(sed -n 's/^## \([^,]*\),.*/\1/p' "$(dirname "$out")/ranked.md" 2> /dev/null | head -n 1)
    echo "mutants on $night, ${crate:-a crate}: $("$here"/mutants-verdict.sh "$out" 2> /dev/null)"
  done
  missed=$(cat "$work"/mutants/mutants.out/missed.txt "$work"/mutants/*/mutants.out/missed.txt 2> /dev/null | grep -c . || true)
  echo "mutants on $night: $missed no test caught, ranked by file in $folder/mutants-$night.md"
else
  echo "mutants: the nightly run $mutants kept nothing to read"
fi
# The week's big codebase, from the latest scheduled weekly run, as `big-codebase-weekly-<date>.md`:
# each step at 100,000 and 200,000 lines, and how each grew against the program.
read -r weekly weekly_conclusion weekly_started <<< "$(gh run list --repo "$repository" --workflow big-codebase-weekly.yml \
  --event schedule --limit 1 --json databaseId,conclusion,createdAt --jq "$how_ended" 2> /dev/null)"
if [ -n "${weekly:-}" ] && [ "$weekly_conclusion" != running ]; then
  week=${weekly_started%%T*}
  if [ ! -f "$folder/big-codebase-weekly-$week.md" ] \
    && gh run download "$weekly" --repo "$repository" --name big-codebase-weekly --dir "$work/weekly" > /dev/null 2>&1; then
    {
      echo "# The big codebase at 100,000 and 200,000 lines, $week: $weekly_conclusion"
      echo
      echo "https://github.com/$repository/actions/runs/$weekly"
      echo
      cat "$work/weekly/big-codebase-weekly.md"
    } > "$folder/big-codebase-weekly-$week.md"
    echo "big codebase weekly on $week: $weekly_conclusion, in $folder/big-codebase-weekly-$week.md"
  fi
fi
read -r run conclusion created <<< "$(gh run list --repo "$repository" --workflow proofs.yml --limit 1 \
  --json databaseId,conclusion,createdAt --jq "$how_ended")"
if [ -z "${run:-}" ]; then
  echo "no Proofs run yet"
  exit $((main_ci | nights))
fi
if [ "$conclusion" = running ]; then
  echo "the Proofs run $run is still going"
  exit $((main_ci | nights))
fi
# Each job of the night that failed, and the step it failed at, so a failed night says plainly
# what failed even when it's no test, as when a soak's memory rose past its budget.
steps=""
if [ "$conclusion" != success ]; then
  steps=$(failed_steps "$run") \
    || steps="  failed: its jobs couldn't be read, see https://github.com/$repository/actions/runs/$run"
fi
if ! gh run download "$run" --repo "$repository" --name nightly-report --dir "$work" > /dev/null 2>&1; then
  echo "the Proofs run $run ($conclusion) kept no report"
  [ -z "$steps" ] || echo "$steps"
  [ "$conclusion" = success ] || exit 1
  exit $((main_ci | nights))
fi
day=${created%%T*}
{
  if [ -n "$steps" ]; then
    echo "# What failed in the Proofs run of $day"
    echo
    echo "https://github.com/$repository/actions/runs/$run"
    echo
    sed 's/^  failed: /- /' <<< "$steps"
    echo
  fi
  cat "$work/nightly.md"
} > "$folder/nightly-proofs-$day.md"
# The soaks' curves from the same night, after the rest: each real program's memory and live
# blocks over an hour, where they started and ended and how fast they rose.
if gh run download "$run" --repo "$repository" --pattern 'soak-*' --dir "$work/soak" > /dev/null 2>&1; then
  cat "$work"/soak/soak.md "$work"/soak/*/soak.md 2> /dev/null >> "$folder/nightly-proofs-$day.md" || true
fi
# The big codebase's steps at each size, and how each grew against the program.
if gh run download "$run" --repo "$repository" --name big-codebase --dir "$work/big" > /dev/null 2>&1; then
  cat "$work/big/big-codebase.md" 2> /dev/null >> "$folder/nightly-proofs-$day.md" || true
fi
# The tests that failed, by name, so a failed night reads in one line, and under it each job and
# step that failed.
failed=$(sed -n 's/^Failed tests: //p' "$work/nightly.md" | head -n 1)
echo "Proofs on $day: $conclusion${failed:+, failed tests: $failed}, report in $folder/nightly-proofs-$day.md"
[ -z "$steps" ] || echo "$steps"
[ "$conclusion" = success ] && [ "$main_ci" -eq 0 ] && [ "$nights" -eq 0 ]
