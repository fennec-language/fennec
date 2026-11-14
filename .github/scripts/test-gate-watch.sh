#!/usr/bin/env bash
# Tries `gate-watch.sh` on programs named like the build tools, which it must leave running, and on
# one that isn't, which it must stop. Run it on a Mac, where the gate runs.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
# A program that sleeps, compiled, since a copied system binary isn't allowed to run elsewhere.
printf '#include <stdlib.h>\n#include <unistd.h>\nint main(int c, char **v) { sleep(atoi(v[1])); return 0; }\n' > "$work/sleeper.c"
# And one that keeps a core busy for as many seconds.
printf '#include <stdlib.h>\n#include <time.h>\nint main(int c, char **v) { time_t end = time(0) + atoi(v[1]); volatile long n = 0; while (time(0) < end) n++; return 0; }\n' > "$work/busy.c"
mkdir -p "$work/bin" "$work/deps"
for name in cargo rustc ld program; do
  cc -o "$work/bin/$name" "$work/sleeper.c" || exit 1
done
cc -o "$work/deps/fennec-0123456789abcdef" "$work/sleeper.c" || exit 1
cc -o "$work/bin/busy" "$work/busy.c" || exit 1
# And one that holds 200 MB and keeps using it for 30 seconds, as a program running away does. One
# that only waited would be paged out while a gate runs, short of memory, until it held almost
# nothing; filled with bytes that repeat, it would be compressed to almost nothing as well.
cat > "$work/swelling.c" << 'EOF'
#include <stdlib.h>
#include <time.h>
#include <unistd.h>
int main(void) {
  long size = 200L << 20;
  unsigned char *held = malloc(size);
  unsigned x = 1;
  for (long at = 0; at < size; at++) { x = x * 1103515245 + 12345; held[at] = x >> 24; }
  time_t end = time(0) + 30;
  while (time(0) < end) {
    for (long at = 0; at < size; at += 4096) held[at]++;
    usleep(100000);
  }
  return held[7];
}
EOF
cc -o "$work/bin/swelling" "$work/swelling.c" || exit 1

# Runs a program for six seconds under a CPU limit of two seconds, and a wall-clock limit of
# `$2`, or twenty seconds.
watched() {
  bash -c "\"$1\" 6; true" &
  local parent=$!
  : > "$work/log"
  "$here/gate-watch.sh" "$parent" "$work/log" 2 4194304 "${2:-20}" &
  local watcher=$!
  wait "$parent"
  wait "$watcher"
}

for tool in "$work/bin/cargo" "$work/bin/rustc" "$work/bin/ld" "$work/deps/fennec-0123456789abcdef"; do
  watched "$tool" 2> /dev/null
  if [ -s "$work/log" ]; then
    echo "FAILED: $(basename "$tool") was stopped: $(cat "$work/log")"
    failures=$((failures + 1))
  else
    echo "ok: $(basename "$tool") runs as long as it needs"
  fi
done

# A program that has exited, whose parent hasn't collected it: `ps` shows it as `(program)`.
bash -c "\"$work/bin/program\" 0 & exec \"$work/bin/cargo\" 6" &
parent=$!
: > "$work/log"
"$here/gate-watch.sh" "$parent" "$work/log" 2 &
watcher=$!
wait "$parent"
wait "$watcher"
if [ -s "$work/log" ]; then
  echo "FAILED: a program that had exited was stopped: $(cat "$work/log")"
  failures=$((failures + 1))
else
  echo "ok: a program that has exited is left alone"
fi

# On a busy machine a core is shared, so this one runs until it has had a second of CPU, which
# it gets in thirty seconds however busy the machine is.
bash -c "\"$work/bin/busy\" 30; true" &
parent=$!
: > "$work/log"
"$here/gate-watch.sh" "$parent" "$work/log" 1 4194304 60 &
watcher=$!
wait "$parent"
wait "$watcher"
if grep -q "over its cpu limit" "$work/log"; then
  echo "ok: a test program over its CPU limit is stopped"
else
  echo "FAILED: a test program over its CPU limit wasn't stopped"
  failures=$((failures + 1))
fi

# Waiting isn't using the CPU, so a program that only waits goes on, up to the wall-clock limit.
watched "$work/bin/program" 2> /dev/null
if [ -s "$work/log" ]; then
  echo "FAILED: a program waiting for less than its wall-clock limit was stopped: $(cat "$work/log")"
  failures=$((failures + 1))
else
  echo "ok: a program that waits isn't stopped for its CPU limit"
fi
watched "$work/bin/program" 3 2> /dev/null
if grep -q "over its time limit" "$work/log"; then
  echo "ok: a program stuck past its wall-clock limit is stopped"
else
  echo "FAILED: a program stuck past its wall-clock limit wasn't stopped"
  failures=$((failures + 1))
fi
# And the log says what it was doing when it was stopped: what it had started, and where it was.
if grep -q "it and what it started:" "$work/log" \
  && { ! command -v sample > /dev/null || grep -q "where it was, sampled for a second:" "$work/log"; }; then
  echo "ok: what a stopped program was doing is kept"
else
  echo "FAILED: the log doesn't say what the stopped program was doing: $(cat "$work/log")"
  failures=$((failures + 1))
fi

# The memory limit holds for every program, a tool's name or not, and comes before any time limit.
for named in swelling cargo; do
  program="$work/bin/swelling"
  [ "$named" = cargo ] && { mkdir -p "$work/tool"; cp "$work/bin/swelling" "$work/tool/cargo"; program="$work/tool/cargo"; }
  bash -c "\"$program\"; true" &
  parent=$!
  : > "$work/log"
  "$here/gate-watch.sh" "$parent" "$work/log" 60 20480 600 &
  watcher=$!
  wait "$parent"
  wait "$watcher"
  if grep -q "over its memory limit" "$work/log"; then
    echo "ok: a program named $named holding more than its memory limit is stopped"
  else
    echo "FAILED: a program named $named holding more than its memory limit wasn't stopped"
    failures=$((failures + 1))
  fi
done

# Time held at launch, as macOS holds a brand-new executable at `_dyld_start`, counts against no
# limit. A launcher starts a program suspended at its first instruction, which is where such a
# hold leaves it, and lets it go after as many seconds as it's given. It sits under a path like a
# test binary's, as the gate's launchers do, so gate-watch leaves it be.
cat > "$work/held.c" << 'EOF'
#include <signal.h>
#include <spawn.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>
extern char **environ;
int main(int argc, char **argv) {
  posix_spawnattr_t attr;
  posix_spawnattr_init(&attr);
  posix_spawnattr_setflags(&attr, POSIX_SPAWN_START_SUSPENDED);
  pid_t pid;
  if (argc < 3 || posix_spawn(&pid, argv[2], NULL, &attr, argv + 2, environ) != 0) return 2;
  sleep(atoi(argv[1]));
  kill(pid, SIGCONT);
  int status;
  waitpid(pid, &status, 0);
  return WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
}
EOF
if [ "$(uname)" = Darwin ] && cc -o "$work/deps/held-0123456789abcdef" "$work/held.c"; then
  launcher="$work/deps/held-0123456789abcdef"
  # Held for 15 seconds against a wall-clock limit of 6, then runs for a moment and ends.
  bash -c "\"$launcher\" 15 \"$work/bin/program\" 0" &
  parent=$!
  : > "$work/log"
  : > "$work/holds"
  "$here/gate-watch.sh" "$parent" "$work/log" 2 4194304 6 "$work/holds" &
  watcher=$!
  wait "$parent"
  wait "$watcher"
  held=$(awk 'NR == 1 { print $1 }' "$work/holds")
  if [ -s "$work/log" ]; then
    echo "FAILED: a program held at launch past its wall-clock limit was stopped: $(cat "$work/log")"
    failures=$((failures + 1))
  elif [ -z "$held" ] || [ "$held" -lt 10 ]; then
    echo "FAILED: a 15-second hold at launch was written as \`$(cat "$work/holds")\`"
    failures=$((failures + 1))
  else
    echo "ok: a program held at launch isn't stopped, and its hold of ${held}s is written"
  fi
  # Held for 10 seconds, then stuck: stopped 6 seconds after it starts.
  bash -c "\"$launcher\" 10 \"$work/bin/program\" 30; true" &
  parent=$!
  : > "$work/log"
  "$here/gate-watch.sh" "$parent" "$work/log" 2 4194304 6 "$work/holds" &
  watcher=$!
  wait "$parent"
  wait "$watcher"
  if grep -q "over its time limit.*s-held-at-launch" "$work/log"; then
    echo "ok: a program stuck after a hold at launch is stopped"
  else
    echo "FAILED: a program stuck after a hold at launch wasn't stopped: $(cat "$work/log")"
    failures=$((failures + 1))
  fi
fi

# A test program waiting on a build it started isn't stuck while the build works: `driver` starts
# `llc`, a compiler that keeps a core busy, and waits for it, longer than the wall-clock limit.
cat > "$work/driver.c" << 'EOF'
#include <sys/wait.h>
#include <unistd.h>
int main(int argc, char **argv) {
  pid_t pid = fork();
  if (pid == 0) { execv(argv[1], argv + 1); return 127; }
  int status;
  waitpid(pid, &status, 0);
  return WIFEXITED(status) ? WEXITSTATUS(status) : 1;
}
EOF
mkdir -p "$work/llvm"
cc -o "$work/bin/driver" "$work/driver.c" || exit 1
cc -o "$work/llvm/llc" "$work/busy.c" || exit 1
bash -c "\"$work/bin/driver\" \"$work/llvm/llc\" 15; true" &
parent=$!
: > "$work/log"
"$here/gate-watch.sh" "$parent" "$work/log" 60 4194304 4 &
watcher=$!
wait "$parent"
wait "$watcher"
if [ -s "$work/log" ]; then
  echo "FAILED: a program waiting on a compiler at work past its wall-clock limit was stopped: $(cat "$work/log")"
  failures=$((failures + 1))
else
  echo "ok: a program waiting on a compiler at work isn't stopped for its wall-clock limit"
fi

# But nothing runs past six times that limit, stuck or not, and what it started is stopped too.
bash -c "\"$work/bin/driver\" \"$work/llvm/llc\" 40; true" &
parent=$!
: > "$work/log"
"$here/gate-watch.sh" "$parent" "$work/log" 60 4194304 2 &
watcher=$!
wait "$parent"
wait "$watcher"
if grep -q "over its ceiling limit" "$work/log" && ! pgrep -f "$work/llvm/llc" > /dev/null; then
  echo "ok: a program past its ceiling is stopped, with what it started"
else
  echo "FAILED: a program past its ceiling wasn't stopped with what it started: $(cat "$work/log")"
  pkill -f "$work/llvm/llc"
  failures=$((failures + 1))
fi

[ "$failures" = 0 ] && echo "all passed"
exit "$failures"
