#!/usr/bin/env bash
# Runs a command for at most so many seconds, so a hang names what hung instead of holding a
# runner. If the command is still going then, this stops it and everything it started, says so,
# and exits with 124:
#
#   .github/scripts/within.sh 120 ./target/debug/fennec run examples/hello.fen
#
# A process the system can't kill, like an x86-64 program while Rosetta is wedged, doesn't hold it
# either: it waits a few seconds for the command to go, then exits with 124 all the same, saying
# it's left running. WITHIN_SIGNAL names another signal to stop with, for its test,
# `test-within.sh`, since nothing can ignore the KILL it sends.
set -u
limit=$1
shift
signal=${WITHIN_SIGNAL:-KILL}
# How long a stopped command has to go.
grace=5

# In a process group of its own, so that everything it started can be stopped with it. It reads
# nothing: a program waiting for input would only wait.
set -m
"$@" < /dev/null &
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

deadline=$((SECONDS + limit))
while kill -0 "$command" 2> /dev/null; do
  if [ "$SECONDS" -ge "$deadline" ]; then
    stop
    echo "::error::\`$*\` didn't finish within $limit seconds" >&2
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
