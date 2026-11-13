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
# And one that holds 200 MB, touched so it's really held, then waits.
printf '#include <stdlib.h>\n#include <string.h>\n#include <unistd.h>\nint main(void) { char *held = malloc(200 << 20); memset(held, 1, 200 << 20); sleep(30); return held[7]; }\n' > "$work/swelling.c"
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

[ "$failures" = 0 ] && echo "all passed"
exit "$failures"
