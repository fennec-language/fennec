#!/usr/bin/env bash
# Runs a command, such as the tests, and fails if at any moment while it runs loopback connections
# waiting out their close hold more than so many local ports:
#
#   .github/scripts/time-wait.sh 3000 cargo test --locked
#
# The side of a connection that closes first waits in TIME_WAIT for a minute or so afterwards,
# holding its local port. When a test's clients close first, each connection holds a port of its
# own from the system's range of them, and a run making many short connections, beside others,
# runs out: "Can't assign requested address". When the server closes first, its connections all
# wait on the one port it listens on. So what's counted is distinct ports, not connections, and a
# count near the range's size means some test's clients close first again. The command's own
# failure comes first; with that, the peak is said, and at a peak over the limit, the ports most
# connections were waiting on then, which name the servers. Windows, where `netstat` says it
# differently, runs the command alone.
set -u
limit=$1
shift
every=2

# The local and foreign address of each loopback connection in TIME_WAIT, one a line.
waiting() {
  if command -v ss > /dev/null 2>&1; then
    # Linux: with a state asked for, the local and peer addresses are the third and fourth columns.
    ss -Htan state time-wait 2> /dev/null | awk '$3 ~ /^(127\.|\[::1\])/ {print $3, $4}'
  else
    # macOS: the fourth and fifth columns, each with its port after the last dot.
    netstat -an -p tcp 2> /dev/null | awk '$6 == "TIME_WAIT" && $4 ~ /^127\./ {print $4, $5}'
  fi
}

# How many distinct local ports those hold.
held() {
  waiting | awk '{n = split($1, at, /[.:]/); print at[n]}' | sort -u | wc -l | tr -d ' '
}

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) exec "$@" ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
echo 0 > "$work/peak"
(
  peak=0
  while :; do
    now=$(held)
    if [ "$now" -gt "$peak" ]; then
      peak=$now
      echo "$peak" > "$work/peak"
      waiting > "$work/at-peak"
    fi
    sleep "$every"
  done
) &
sampler=$!
"$@"
status=$?
kill "$sampler" 2> /dev/null
wait "$sampler" 2> /dev/null
peak=$(cat "$work/peak")
if [ "$status" -ne 0 ]; then
  exit "$status"
fi
if [ "$peak" -gt "$limit" ]; then
  echo "::error::at one point $peak local ports were held by loopback connections waiting out their close, more than $limit: some test's clients close first, each holding a port of its own. The ports most were waiting on then, with how many:" >&2
  awk '{n = split($2, at, /[.:]/); print at[n]}' "$work/at-peak" | sort | uniq -c | sort -rn | head -5 >&2
  exit 1
fi
echo "at most $peak local ports were held by loopback connections waiting out their close, within $limit"
