# Sourced, not run: whether a job of a run never got a runner, which `ci-passed.sh` says plainly
# and `rerun-no-runner.sh` runs again. GitHub's macOS runners run short, and a job none takes waits
# up to 40 minutes, then is cancelled with no runner and no steps, its check noting "The job was
# not acquired by Runner of type hosted even after multiple attempts".
#
#   never_ran <job id> <conclusion> <runner name> <steps>
#
# needs `gh`, with a token that reads checks, and GITHUB_REPOSITORY.

# A job cancelled before it started, as when its run was, has no runner and no steps either, but
# no such note, so the note decides.
never_ran() {
  local id=$1 conclusion=$2 runner=$3 steps=$4
  local said
  [ "$conclusion" = cancelled ] && [ -z "$runner" ] && [ "$steps" = 0 ] || return 1
  said=$(gh api "repos/$GITHUB_REPOSITORY/check-runs/$id/annotations" --jq '.[].message') || return 1
  grep -q 'was not acquired by Runner' <<< "$said"
}
