#!/usr/bin/env bash
# Watches everything a process started, and stops a test program that runs too long or holds too
# much memory, so that one hanging or swelling program fails the gate instead of taking the Mac
# with it. The build tools and the test binaries themselves can take as long as they need.
#
#   gate-watch.sh ROOT_PID LOG [SECONDS] [KILOBYTES]
set -u
root=$1
log=$2
limit_seconds=${3:-60}
limit_kb=${4:-4194304}

while kill -0 "$root" 2> /dev/null; do
  ps -axo pid=,ppid=,rss=,etime=,command= | awk -v root="$root" -v seconds="$limit_seconds" -v kb="$limit_kb" '
    function age(text, parts, count, days) {
      days = 0
      if (index(text, "-")) { split(text, parts, "-"); days = parts[1]; text = parts[2] }
      count = split(text, parts, ":")
      if (count == 3) return days * 86400 + parts[1] * 3600 + parts[2] * 60 + parts[3]
      return days * 86400 + parts[1] * 60 + parts[2]
    }
    {
      pid = $1; parent[pid] = $2; rss[pid] = $3; elapsed[pid] = $4
      line = $0; sub(/^ *[0-9]+ +[0-9]+ +[0-9]+ +[0-9:-]+ +/, "", line); command[pid] = line
    }
    END {
      for (pid in parent) {
        at = pid
        while (at != root && at in parent && at > 1) at = parent[at]
        if (at != root || pid == root) continue
        text = command[pid]
        tool = text ~ /(^|\/)(cargo|rustc|rustup|rustdoc|cc|ld|clang|ar|as|bash|sh|zsh|awk|ps|sleep|tee|nice|git)( |$)/ \
          || text ~ /\/deps\/[^ ]+-[0-9a-f]+( |$)/ || text ~ /build-script-build/ || text ~ / daemon( |$)/
        if (rss[pid] > kb && text !~ /(^|\/)rustc( |$)/) print pid, "memory", rss[pid], text
        else if (!tool && age(elapsed[pid]) > seconds) print pid, "time", elapsed[pid], text
      }
    }' | while read -r pid why amount text; do
    echo "gate-watch: stopped pid $pid over its $why limit ($amount): ${text:0:200}" >> "$log"
    kill -9 "$pid" 2> /dev/null
  done
  sleep 2
done
