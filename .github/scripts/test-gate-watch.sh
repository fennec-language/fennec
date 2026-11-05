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
mkdir -p "$work/bin" "$work/deps"
for name in cargo rustc ld program; do
  cc -o "$work/bin/$name" "$work/sleeper.c" || exit 1
done
cc -o "$work/deps/fennec-0123456789abcdef" "$work/sleeper.c" || exit 1

watched() {
  bash -c "\"$1\" 6; true" &
  local parent=$!
  : > "$work/log"
  "$here/gate-watch.sh" "$parent" "$work/log" 2 &
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

watched "$work/bin/program" 2> /dev/null
if grep -q "over its time limit" "$work/log"; then
  echo "ok: a test program over the limit is stopped"
else
  echo "FAILED: a test program over the limit wasn't stopped"
  failures=$((failures + 1))
fi

[ "$failures" = 0 ] && echo "all passed"
exit "$failures"
