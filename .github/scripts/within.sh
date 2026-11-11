#!/usr/bin/env bash
# Runs a command with at most so many seconds of CPU time, so a hang names what hung instead of
# holding a runner:
#
#   .github/scripts/within.sh 120 ./target/debug/fennec run examples/hello.fen
#
# It's CPU time, not time by the clock, so a busy machine, which only makes a command wait, never
# fails one: a command that spends two minutes of CPU is in a loop it never leaves. Each process
# the command starts gets as much. One that waits for something that never comes spends none, so
# the clock stops it too, at WITHIN_WALL seconds, ten minutes unless it's set, and never sooner
# than the CPU time. Either way this stops the command and everything it started, says which limit
# it reached, and exits with 124. On Windows, which has no CPU limit to set, the seconds are by the
# clock.
#
# A process the system can't kill, like an x86-64 program while Rosetta is wedged, doesn't hold it
# either: it waits a few seconds for the command to go, then exits with 124 all the same, saying
# it's left running. WITHIN_SIGNAL names another signal to stop with, for its test,
# `test-within.sh`, since nothing can ignore the KILL it sends.
set -u
limit=$1
shift
signal=${WITHIN_SIGNAL:-KILL}
wall=${WITHIN_WALL:-600}
[ "$wall" -ge "$limit" ] || wall=$limit
# What a process stopped for spending its CPU time ends with: 128 and SIGXCPU's 24.
spent=152
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*)
    cpu=no
    wall=$limit
    ;;
  *) cpu=yes ;;
esac
# How long a stopped command has to go.
grace=5

# In a process group of its own, so that everything it started can be stopped with it. It reads
# nothing: a program waiting for input would only wait.
set -m
if [ "$cpu" = yes ]; then
  (ulimit -t "$limit" && exec "$@") < /dev/null &
else
  "$@" < /dev/null &
fi
command=$!
set +m

stop() {
  if [ -e "/proc/$command/winpid" ]; then
    # Windows: what `kill` could reach is the one process, not what it started.
    taskkill //F //T //PID "$(cat "/proc/$command/winpid")" > /dev/null 2>&1
  else
    kill -"$signal" -- "-$command" 2> /dev/null
  fi
}

deadline=$((SECONDS + wall))
while kill -0 "$command" 2> /dev/null; do
  if [ "$SECONDS" -ge "$deadline" ]; then
    stop
    echo "::error::\`$*\` didn't finish within $wall seconds by the clock" >&2
    gone=$((SECONDS + grace))
    while kill -0 "$command" 2> /dev/null; do
      if [ "$SECONDS" -ge "$gone" ]; then
        echo "::error::and it didn't stop when killed, so it's left running as process $command" >&2
        exit 124
      fi
      sleep 0.1
    done
    wait "$command" 2> /dev/null
    exit 124
  fi
  sleep 0.1
done
wait "$command"
status=$?
if [ "$cpu" = yes ] && [ "$status" -eq "$spent" ]; then
  stop
  echo "::error::\`$*\` used all $limit seconds of CPU time it may" >&2
  exit 124
fi
exit "$status"
