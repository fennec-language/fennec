#!/usr/bin/env bash
# gate-lock.sh take PID | release PID: the lock that keeps a worktree to one gate at a time, held
# by the process PID. Two gates in one worktree build and test in the same target folder, and each
# corrupts the other: on 2026-11-20 one's runtime build removed the runtime library while the
# other's tests linked against it, and the failure read as a flaky test (#2294).
#
# `take` holds it for PID, or finds PID holds it already, or refuses at once, naming the process
# that holds it. A lock whose process is gone, as when a gate was killed partway, is taken over.
# `release` lets it go, if PID holds it. The lock is a link in the worktree's own git folder whose
# target is the holder's pid: making a link is all at once, holder and all, so two gates taking it
# together can't both get it. A gate in another worktree doesn't wait on it.
set -euo pipefail
what=${1:?take or release}
pid=${2:?and the process that holds it}
lock="$(git rev-parse --absolute-git-dir)/fennec-gate.lock"
case "$what" in
  take)
    for _ in 1 2; do
      if ln -s "$pid" "$lock" 2> /dev/null; then
        exit 0
      fi
      holder=$(readlink "$lock" 2> /dev/null || true)
      [ "$holder" != "$pid" ] || exit 0
      if [ -n "$holder" ] && ps -p "$holder" > /dev/null 2>&1; then
        echo "error: a gate is already running in this worktree (pid $holder): wait for it, or stop it, before starting another" >&2
        exit 1
      fi
      # Its gate is gone: the lock is let go, and taken again once more.
      rm -f "$lock"
    done
    echo "error: couldn't take the gate's lock $lock" >&2
    exit 1
    ;;
  release)
    if [ "$(readlink "$lock" 2> /dev/null || true)" = "$pid" ]; then
      rm -f "$lock"
    fi
    ;;
  *)
    echo "gate-lock.sh: take or release, not $what" >&2
    exit 2
    ;;
esac
