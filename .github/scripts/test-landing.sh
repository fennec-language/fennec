#!/usr/bin/env bash
# Tries the pre-push hook and `fennec-land` against a throwaway repository, with a gate that does
# as it's told, so that a mistake in them can't reach `staging` first.
set -u
here="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d)"
trap '/bin/rm -rf "$work"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com
export FENNEC_GATE_LOCK="$work/lock" GATE_RAN="$work/gate-ran"
failures=0

check() {
  if [ "$2" = "$3" ]; then
    echo "ok: $1"
  else
    echo "FAILED: $1: expected '$2', got '$3'"
    failures=$((failures + 1))
  fi
}

git init --quiet --bare --initial-branch=main "$work/origin.git"
git clone --quiet "$work/origin.git" "$work/a" 2> /dev/null
cd "$work/a" || exit 1
git config core.hooksPath .githooks
mkdir -p .githooks .github/scripts
cp "$here/.githooks/pre-push" .githooks/
cp "$here/.github/scripts/fennec-land" .github/scripts/
cat > .github/scripts/gate.sh <<'GATE'
#!/usr/bin/env bash
echo ran >> "$GATE_RAN"
if [ -n "${RIVAL:-}" ] && [ ! -e "$RIVAL.raced" ]; then
  : > "$RIVAL.raced"
  git -C "$RIVAL" fetch --quiet origin
  git -C "$RIVAL" checkout --quiet -B rival origin/staging
  echo theirs > "$RIVAL/rival"
  git -C "$RIVAL" add -A
  git -C "$RIVAL" commit --quiet -m rival
  git -C "$RIVAL" push --quiet --no-verify origin rival:staging
fi
[ -z "${GATE_FAILS:-}" ]
GATE
chmod +x .github/scripts/gate.sh
echo one > file
git add -A
git commit --quiet -m one
git push --quiet --no-verify origin HEAD:main HEAD:staging 2> /dev/null
git clone --quiet "$work/origin.git" "$work/b" 2> /dev/null
git -C "$work/b" config core.hooksPath .githooks

staging() { git ls-remote "$work/origin.git" refs/heads/staging | cut -f1; }
ran() { wc -l < "$GATE_RAN" | tr -d ' '; }
new_branch() {
  git fetch --quiet origin
  git checkout --quiet -B "$1" origin/staging
  echo "$1" > "$1"
  git add -A
  git commit --quiet -m "$1"
  : > "$GATE_RAN"
}

new_branch elsewhere
git push --quiet origin elsewhere 2> /dev/null
check "another branch goes straight through" "0 0" "$? $(ran)"

new_branch lands
git push --quiet origin HEAD:staging 2> /dev/null
check "a commit that passes lands" "0 $(git rev-parse HEAD)" "$? $(staging)"
check "and it ran the gate" 1 "$(ran)"

new_branch fails
before="$(staging)"
GATE_FAILS=1 git push --quiet origin HEAD:staging 2> /dev/null
check "a commit that fails the gate is refused" "1 $before" "$? $(staging)"

new_branch dirty
echo more > dirty
git push --quiet origin HEAD:staging 2> /dev/null
check "a tree with changes is refused" "1 $before" "$? $(staging)"
check "before the gate runs" 0 "$(ran)"
git checkout --quiet -- dirty

new_branch aside
git checkout --quiet -B elsewhere origin/staging
git push --quiet origin aside:staging 2> /dev/null
check "a commit that isn't checked out is refused" "1 $before" "$? $(staging)"

# `b` is behind by the time it pushes.
cd "$work/b" || exit 1
git fetch --quiet origin
git checkout --quiet -B behind origin/main
echo behind > behind
git add -A
git commit --quiet -m behind
: > "$GATE_RAN"
git push --quiet origin HEAD:staging 2> /dev/null
check "a commit without the latest staging is refused" "1 $before" "$? $(staging)"
check "before the gate runs" 0 "$(ran)"

.github/scripts/fennec-land > /dev/null 2>&1
check "fennec-land merges the latest staging and lands" "0 $(git rev-parse HEAD)" "$? $(staging)"
git merge-base --is-ancestor "$before" "$(staging)"
check "with what was on staging" 0 "$?"

cd "$work/a" || exit 1
new_branch land_fails
before="$(staging)"
GATE_FAILS=1 .github/scripts/fennec-land > /dev/null 2>&1
check "fennec-land stops when the gate fails" "1 $before" "$? $(staging)"
check "after one try" 1 "$(ran)"

echo changed > land_fails
.github/scripts/fennec-land > /dev/null 2>&1
check "fennec-land stops on uncommitted changes" "1 $before" "$? $(staging)"
git checkout --quiet -- land_fails

new_branch conflict
echo mine > same
git add -A
git commit --quiet -m mine
git -C "$work/b" fetch --quiet origin
git -C "$work/b" checkout --quiet -B theirs origin/staging
echo theirs > "$work/b/same"
git -C "$work/b" add -A
git -C "$work/b" commit --quiet -m theirs
git -C "$work/b" push --quiet --no-verify origin theirs:staging
before="$(staging)"
.github/scripts/fennec-land > /dev/null 2> "$work/said"
check "a conflict stops it, and pushes nothing" "1 $before" "$? $(staging)"
grep -q "conflicts with staging" "$work/said"
check "and says so" 0 "$?"
git merge --abort

new_branch racer
RIVAL="$work/b" .github/scripts/fennec-land > /dev/null 2>&1
check "losing the race goes round again and lands" "0 $(git rev-parse HEAD)" "$? $(staging)"
check "after two gates" 2 "$(ran)"
git merge-base --is-ancestor "$(git -C "$work/b" rev-parse rival)" "$(staging)"
check "with the rival's commit" 0 "$?"
check "and the lock is gone" "" "$(ls -d "$FENNEC_GATE_LOCK" 2> /dev/null)"

mkdir "$FENNEC_GATE_LOCK"
echo 999999 > "$FENNEC_GATE_LOCK/pid"
new_branch dead_lock
git push --quiet origin HEAD:staging 2> /dev/null
check "a lock left by a dead process is taken over" "0 $(git rev-parse HEAD)" "$? $(staging)"

if [ "$failures" -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "all passed"
