#!/usr/bin/env bash
# Watches everything a process started, and stops a test program that runs too long or holds too
# much memory, so that one hanging or swelling program fails the gate instead of taking the Mac
# with it. The compiler, cargo, the linker and the test binaries themselves can take as long as
# they need, though nothing can hold more than the memory limit.
#
# A test program's limit is CPU time, so a machine busy with other builds, where a program waits
# for a core, can't make one that isn't stuck look stuck. One stuck without using the CPU, like a
# deadlock, is stopped once it has been running ten times that long.
#
#   gate-watch.sh ROOT_PID LOG [CPU_SECONDS] [KILOBYTES] [WALL_SECONDS]
set -u
root=$1
log=$2
limit_seconds=${3:-60}
limit_kb=${4:-4194304}
limit_wall=${5:-$((limit_seconds * 10))}

while kill -0 "$root" 2> /dev/null; do
  # A tool is known by its name, `ucomm`, which stays when a process that's exiting has lost its
  # arguments and shows only as `(cargo)`.
  ps -axo pid=,ppid=,rss=,etime=,time=,ucomm=,command= | awk -v root="$root" -v seconds="$limit_seconds" -v kb="$limit_kb" -v wall="$limit_wall" '
    function age(text, parts, count, days) {
      days = 0
      if (index(text, "-")) { split(text, parts, "-"); days = parts[1]; text = parts[2] }
      count = split(text, parts, ":")
      if (count == 3) return days * 86400 + parts[1] * 3600 + parts[2] * 60 + parts[3]
      return days * 86400 + parts[1] * 60 + parts[2]
    }
    {
      pid = $1; parent[pid] = $2; rss[pid] = $3; elapsed[pid] = $4; cpu[pid] = $5; name[pid] = $6
      line = $0; sub(/^ *[0-9]+ +[0-9]+ +[0-9]+ +[0-9:-]+ +[0-9:.-]+ +[^ ]+ +/, "", line); command[pid] = line
    }
    END {
      for (pid in parent) {
        at = pid
        while (at != root && at in parent && at > 1) at = parent[at]
        if (at != root || pid == root) continue
        text = command[pid]
        # A process that has exited shows as `(name)` or `<defunct>` until whatever started it
        # collects it. It holds no time and no memory any more, and so needs no stopping.
        if (text ~ /^\(.*\)$/ || text ~ /<defunct>/) continue
        tool = name[pid] ~ /^(cargo|cargo-clippy|clippy-driver|rustc|rustup|rustdoc|cc|c\+\+|clang|ld|ld64|ld64\.lld|lld|rust-lld|ar|as|xcrun|bash|sh|zsh|awk|ps|sleep|tee|nice|git)$/ \
          || text ~ /\/deps\/[^ ]+-[0-9a-f]+( |$)/ || text ~ /build-script-build/ || text ~ / daemon( |$)/
        if (rss[pid] > kb) print pid, "memory", rss[pid], text
        else if (!tool && age(cpu[pid]) > seconds) print pid, "cpu", cpu[pid], text
        else if (!tool && age(elapsed[pid]) > wall) print pid, "time", elapsed[pid], text
      }
    }' | while read -r pid why amount text; do
    # What it was doing, before it's stopped: which limit, how long and how big it had grown,
    # what it had started, and where it was, by a second's sampling on a Mac, or what the kernel
    # says it waits on elsewhere. A program stopped with no output says nothing of its own.
    {
      echo "gate-watch: stopped pid $pid over its $why limit ($amount) at $(date '+%H:%M:%S'): ${text:0:200}"
      echo "  it and what it started:"
      ps -axo pid=,ppid=,stat=,etime=,time=,rss=,command= | awk -v top="$pid" '
        { parent[$1] = $2; line[$1] = $0 }
        END {
          for (at in parent) {
            up = at
            while (up != top && up in parent && up > 1) up = parent[up]
            if (up == top) print "    " substr(line[at], 1, 200)
          }
        }'
      if command -v sample > /dev/null; then
        echo "  where it was, sampled for a second:"
        sample "$pid" 1 -mayDie 2> /dev/null | sed -n '/^Call graph:/,/^Total number in stack/p' | head -60 | sed 's/^/    /'
      elif [ -r "/proc/$pid/wchan" ]; then
        echo "  what it waits on: $(cat "/proc/$pid/wchan" 2> /dev/null), $(cut -d' ' -f3 "/proc/$pid/stat" 2> /dev/null)"
      fi
    } >> "$log" 2>&1
    kill -9 "$pid" 2> /dev/null
  done
  sleep 2
done
