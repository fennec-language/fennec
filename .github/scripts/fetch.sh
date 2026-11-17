#!/usr/bin/env bash
# Fetches from origin under a lock every worktree of this repository shares, passing on what
# follows `origin`: `fetch.sh main`. Worktrees share one store of refs, so two gates that fetched
# at once raced on origin/main's ref lock, and the one that lost failed before it began: "cannot
# lock ref 'refs/remotes/origin/main'". One at a time, neither does. The lock is held while git
# runs, not handed to it: perl marks the files it opens close-on-exec, so an exec would let it go.
set -euo pipefail
lock="$(git rev-parse --path-format=absolute --git-common-dir)/fennec-fetch.lock"
perl -MFcntl=:flock -e '
  open(my $held, ">>", shift) or die "fetch.sh: no lock file: $!\n";
  flock($held, LOCK_EX) or die "fetch.sh: no lock: $!\n";
  system(@ARGV);
  exit($? == -1 ? 127 : ($? & 127) ? 128 + ($? & 127) : $? >> 8);
' "$lock" git fetch --quiet origin "$@"
