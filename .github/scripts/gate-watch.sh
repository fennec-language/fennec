#!/usr/bin/env bash
# Watches everything a process started, and stops a test program that runs too long or holds too
# much memory, so that one hanging or swelling program fails the gate instead of taking the Mac
# with it. The compiler, cargo, LLVM's tools, the linker and the test binaries themselves can take
# as long as they need, though nothing can hold more than the memory limit.
#
# A test program's limit is CPU time, so a machine busy with other builds, where a program waits
# for a core, can't make one that isn't stuck look stuck. One stuck without using the CPU, like a
# deadlock, is stopped once it and everything it started have used no CPU for ten times that long.
# A program waiting on a build it started, which is compiling, isn't stuck, however busy the machine
# makes that build: on 2026-11-14 a fuzz test was stopped after 10 minutes while its `llc` worked
# through a release build at a load of 75. Nothing, stuck or not, runs past six times that, an hour
# by default. A program stopped is stopped with everything it started.
#
# Time a program spends held at launch doesn't count against that. macOS can hold a brand-new
# executable at its first instruction, `_dyld_start`, while it's assessed, for seconds or for
# minutes when the machine is busy. A held program has used no CPU and holds almost no memory, but
# so does a small program idle in a wait, once macOS has compressed its pages. So a program that
# looks like that for two seconds is sampled once. When the sample finds it at `_dyld_start`, it's
# held, and its time counts only from when it starts. A program still held is sampled again every
# minute, so one that starts and then hangs is still stopped. A sample finding it anywhere else
# means it has started, and it's never sampled again.
#
# Each hold of two seconds or more is written to HOLDS, a line each: the seconds, then the
# program. Elsewhere than macOS there are no holds, and nothing is sampled.
#
#   gate-watch.sh ROOT_PID LOG [CPU_SECONDS] [KILOBYTES] [WALL_SECONDS] [HOLDS]
set -u
root=$1
log=$2
limit_seconds=${3:-60}
limit_kb=${4:-4194304}
limit_wall=${5:-$((limit_seconds * 10))}
holds=${6:-/dev/null}
sampling=0
command -v sample > /dev/null && sampling=1
# What's known of each program from one look to the next, a line each: its pid and name, so a pid
# used again starts afresh; `unknown`, `held` or `ran`; the seconds it was held; how long it had
# been running when last sampled; whether its hold has been written; the CPU seconds it and what
# it started had used; how long it had been running when that last changed; and the program. Samples
# taken since the last look go in `$state.samples`: the pid, the name, `held` or `ran`, and when.
state=$(mktemp)
sampled=$(mktemp)
trap 'rm -f "$state" "$state.next" "$state.samples" "$state.samples.next" "$sampled"' EXIT

while kill -0 "$root" 2> /dev/null; do
  # A tool is known by its name, `ucomm`, which stays when a process that's exiting has lost its
  # arguments and shows only as `(cargo)`.
  ps -axo pid=,ppid=,rss=,etime=,time=,ucomm=,command= | awk -v root="$root" -v seconds="$limit_seconds" -v kb="$limit_kb" -v wall="$limit_wall" -v state="$state" -v holds="$holds" -v sampling="$sampling" '
    function age(text, parts, count, days) {
      days = 0
      if (index(text, "-")) { split(text, parts, "-"); days = parts[1]; text = parts[2] }
      count = split(text, parts, ":")
      if (count == 3) return days * 86400 + parts[1] * 3600 + parts[2] * 60 + parts[3]
      return days * 86400 + parts[1] * 60 + parts[2]
    }
    BEGIN {
      while ((getline kept < state) > 0) {
        split(kept, field, " ")
        was[field[1]] = field[2]; verdict[field[1]] = field[3]; held[field[1]] = field[4]
        probed[field[1]] = field[5]; written[field[1]] = field[6]
        used[field[1]] = field[7]; moved[field[1]] = field[8]
        program[field[1]] = kept; sub(/^([^ ]+ ){8}/, "", program[field[1]])
      }
      close(state)
      while ((getline kept < (state ".samples")) > 0) {
        split(kept, field, " ")
        if (!(field[1] in was) || was[field[1]] != field[2]) continue
        if (field[3] == "held") {
          verdict[field[1]] = "held"; held[field[1]] = field[4]; probed[field[1]] = field[4]
        } else {
          # Started: held only as long as the last sample that found it held.
          verdict[field[1]] = "ran"; held[field[1]] = probed[field[1]]
        }
      }
      close(state ".samples")
    }
    {
      pid = $1; parent[pid] = $2; rss[pid] = $3; elapsed[pid] = $4; cpu[pid] = $5; name[pid] = $6
      line = $0; sub(/^ *[0-9]+ +[0-9]+ +[0-9]+ +[0-9:-]+ +[0-9:.-]+ +[^ ]+ +/, "", line); command[pid] = line
    }
    END {
      next_state = state ".next"
      printf "" > next_state
      # The CPU seconds each process and everything it started have used, those still running.
      for (pid in parent) {
        at = pid
        while (at != root && at in parent && at > 1) at = parent[at]
        if (at != root) continue
        at = pid
        while (1) {
          tree[at] += age(cpu[pid])
          if (at == root || !(at in parent) || at <= 1) break
          at = parent[at]
        }
      }
      for (pid in parent) {
        at = pid
        while (at != root && at in parent && at > 1) at = parent[at]
        if (at != root || pid == root) continue
        text = command[pid]
        # A process that has exited shows as `(name)` or `<defunct>` until whatever started it
        # collects it. It holds no time and no memory any more, and so needs no stopping.
        if (text ~ /^\(.*\)$/ || text ~ /<defunct>/) continue
        present[pid] = 1
        tool = name[pid] ~ /^(cargo|cargo-clippy|clippy-driver|rustc|rustup|rustdoc|cc|c\+\+|clang|ld|ld64|ld64\.lld|lld|rust-lld|ar|as|xcrun|opt|llc|llvm-link|llvm-ar|bash|sh|zsh|awk|ps|sleep|tee|nice|git|sample)$/ \
          || text ~ /\/deps\/[^ ]+-[0-9a-f]+( |$)/ || text ~ /build-script-build/ || text ~ / daemon( |$)/
        if (rss[pid] > kb) { print "stop", pid, "memory", rss[pid], text; continue }
        if (tool) continue
        known = pid in was && was[pid] == name[pid]
        so = known ? verdict[pid] : (sampling ? "unknown" : "ran")
        for_seconds = known ? held[pid] : 0
        sampled_at = known ? probed[pid] : 0
        told = known ? written[pid] : 0
        running = age(elapsed[pid])
        sample = 0
        if (cpu[pid] !~ /[1-9]/ && rss[pid] < 1024) {
          if (so == "held") {
            for_seconds = running
            sample = running - sampled_at >= 60
          } else if (so == "unknown") {
            sample = running >= 2
          }
        } else if (so != "ran") {
          # It has used the CPU or grown: started, after however long the last look found it held.
          so = "ran"
        }
        if (so == "ran" && for_seconds >= 2 && !told) {
          print for_seconds, substr(text, 1, 200) >> holds
          told = 1
        }
        # When it, or anything it started, last used the CPU: any change in what they had used, since
        # one that ended takes its share away.
        moved_at = known ? moved[pid] : 0
        if (!known || tree[pid] != used[pid]) moved_at = running
        quiet = running - (moved_at > for_seconds ? moved_at : for_seconds)
        print pid, name[pid], so, for_seconds, sampled_at, told, tree[pid], moved_at, substr(text, 1, 200) >> next_state
        if (sample) print "sample", pid, name[pid], running
        else if (age(cpu[pid]) > seconds) print "stop", pid, "cpu", cpu[pid], text
        else if (quiet > wall) print "stop", pid, "time", elapsed[pid] "-with-" for_seconds "s-held-at-launch-and-" quiet "s-with-no-CPU", text
        else if (running - for_seconds > wall * 6) print "stop", pid, "ceiling", elapsed[pid] "-with-" for_seconds "s-held-at-launch", text
      }
      # One that was held and has gone since the last look started and ended in between.
      for (pid in was) {
        if (pid in present && was[pid] == name[pid]) continue
        if (verdict[pid] != "unknown" && held[pid] >= 2 && !written[pid]) print held[pid], program[pid] >> holds
      }
      close(next_state)
      if (holds != "/dev/null") close(holds)
    }' | while read -r what pid why amount text; do
    if [ "$what" = sample ]; then
      # `why` is its name and `amount` how long it had been running. Its first frame, from a
      # second's sampling, says whether it has started.
      found=ran
      if sample "$pid" 1 1 -mayDie -file "$sampled" > /dev/null 2>&1 \
        && sed -n '/^Call graph:/,/^$/p' "$sampled" | sed -n 3p | grep -q '_dyld_start'; then
        found=held
      fi
      echo "$pid $why $found $amount" >> "$state.samples.next"
      continue
    fi
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
      if [ "$sampling" = 1 ]; then
        echo "  where it was, sampled for a second:"
        sample "$pid" 1 -mayDie -file "$sampled" > /dev/null 2>&1
        sed -n '/^Call graph:/,/^Total number in stack/p' "$sampled" | head -60 | sed 's/^/    /'
      elif [ -r "/proc/$pid/wchan" ]; then
        echo "  what it waits on: $(cat "/proc/$pid/wchan" 2> /dev/null), $(cut -d' ' -f3 "/proc/$pid/stat" 2> /dev/null)"
      fi
    } >> "$log" 2>&1
    # It and everything it started, which would otherwise go on with no one to wait for them.
    for each in $(ps -axo pid=,ppid= | awk -v top="$pid" '
      { parent[$1] = $2 }
      END {
        for (at in parent) {
          up = at
          while (up != top && up in parent && up > 1) up = parent[up]
          if (up == top) print at
        }
      }'); do
      kill -9 "$each" 2> /dev/null
    done
  done
  mv -f "$state.next" "$state" 2> /dev/null
  rm -f "$state.samples"
  [ -f "$state.samples.next" ] && mv -f "$state.samples.next" "$state.samples"
  sleep 2
done
# Holds of programs that ended with everything else, since the last look.
awk -v holds="$holds" '$3 == "held" && $4 >= 2 && !$6 { line = $0; sub(/^([^ ]+ ){8}/, "", line); print $4, line >> holds }' "$state"
