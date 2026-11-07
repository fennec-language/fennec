#!/usr/bin/env bash
# Tries `open-pr.sh` against a throwaway repository, with a gate and a `gh` that do as they're told,
# so that a mistake in it can't push a branch that conflicts with main or failed its gate.
set -u
here="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d)"
trap '/bin/rm -rf "$work"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com
export GATE="$work/gate" GH="$work/gh" OPENED="$work/opened"
failures=0

check() {
  if [ "$2" = "$3" ]; then
    echo "ok: $1"
  else
    echo "FAILED: $1: expected '$2', got '$3'"
    failures=$((failures + 1))
  fi
}

# Both are written as they'll run, so what's in them expands when they do.
# shellcheck disable=SC2016
printf '#!/usr/bin/env bash\n[ -z "${GATE_FAILS:-}" ] || { echo "FAILED: tests"; exit 1; }\necho gate passed\n' > "$GATE"
# shellcheck disable=SC2016
printf '#!/usr/bin/env bash\necho "$*" >> "$OPENED"\n' > "$GH"
chmod +x "$GATE" "$GH"
echo "What it does." > "$work/body.md"

git init --quiet --bare --initial-branch=main "$work/origin.git"
git clone --quiet "$work/origin.git" "$work/a" 2> /dev/null
cd "$work/a" || exit 1
mkdir -p .github/scripts
cp "$here/.github/scripts/open-pr.sh" .github/scripts/
printf 'one\ntwo\n' > shared
git add -A
git commit --quiet -m base
git push --quiet origin HEAD:main

pushed() {
  git ls-remote origin "refs/heads/$1" | cut -f1
}

# Each branch starts from the base, before main moves on.
branch_from_base() {
  git checkout --quiet -B "$1" origin/main
}

branch_from_base conflicting
printf 'one\nours\n' > shared
git commit --quiet -am ours
branch_from_base marked
printf 'a\n<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> theirs\n' > left
git add left
git commit --quiet -m "a marker left in"
branch_from_base failing
echo x > own
git add own
git commit --quiet -m "fails its gate"
branch_from_base clean
echo y > own
git add own
git commit --quiet -m "its own file"
git checkout --quiet --detach origin/main
printf 'one\nmain\n' > shared
git commit --quiet -am "main moves on"
git push --quiet origin HEAD:main

git checkout --quiet conflicting
before="$(git rev-parse HEAD)"
.github/scripts/open-pr.sh "Conflicting" "$work/body.md" > /dev/null 2> "$work/said"
check "a branch that conflicts with main stops it, and pushes nothing" "1 " "$? $(pushed conflicting)"
grep -q "merging main conflicts" "$work/said"
check "and says so" 0 "$?"
check "the merge is undone" "$before" "$(git rev-parse HEAD)"
check "with nothing left half merged" "no merge " "$(test -e .git/MERGE_HEAD && echo merging || echo no merge) $(git status --porcelain --untracked-files=no)"

git checkout --quiet marked
.github/scripts/open-pr.sh "Marked" "$work/body.md" > /dev/null 2> "$work/said"
check "a conflict marker left in a file stops it, and pushes nothing" "1 " "$? $(pushed marked)"
grep -q "conflict markers are left" "$work/said"
check "and says so" 0 "$?"

git checkout --quiet failing
GATE_FAILS=1 .github/scripts/open-pr.sh "Failing" "$work/body.md" > /dev/null 2> "$work/said"
check "a gate that fails stops it, and pushes nothing" "1 " "$? $(pushed failing)"

git checkout --quiet main 2> /dev/null || git checkout --quiet -B main origin/main
.github/scripts/open-pr.sh "On main" "$work/body.md" > /dev/null 2> "$work/said"
check "on main it stops" 1 "$?"

git checkout --quiet clean
echo uncommitted >> own
.github/scripts/open-pr.sh "Uncommitted" "$work/body.md" > /dev/null 2> "$work/said"
check "changes not committed stop it, and push nothing" "1 " "$? $(pushed clean)"
git checkout --quiet -- own

check "no pull request was opened for any of those" "" "$(cat "$OPENED" 2> /dev/null)"

.github/scripts/open-pr.sh "Clean" "$work/body.md" > /dev/null 2> "$work/said"
check "a clean branch is pushed at its own commit" "0 $(git rev-parse HEAD)" "$? $(pushed clean)"
git merge-base --is-ancestor origin/main HEAD
check "with main merged in" 0 "$?"
check "and its pull request opened" "pr create --base main --head clean --title Clean --body-file $work/body.md" "$(cat "$OPENED")"

if [ "$failures" -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "all passed"
