#!/usr/bin/env bash
# Runs a command for at most so many seconds, so a hang names what hung instead of holding a
# runner. If the command is still going then, this stops it and everything it started, says so,
# and exits with 124:
#
#   .github/scripts/within.sh 120 ./target/debug/fennec run examples/hello.fen
set -u
limit=$1
shift

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
    kill -KILL -- "-$command" 2> /dev/null
  fi
}

deadline=$((SECONDS + limit))
while kill -0 "$command" 2> /dev/null; do
  if [ "$SECONDS" -ge "$deadline" ]; then
    stop
    wait "$command" 2> /dev/null
    echo "::error::\`$*\` didn't finish within $limit seconds" >&2
    exit 124
  fi
  sleep 0.1
done
wait "$command"
