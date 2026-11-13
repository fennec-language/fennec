#!/usr/bin/env bash
# Reads .github/known-failures.txt, holding every line to its form, `<exact name> | <change>`, where
# the change is a pull request, `#1609`, or a commit, `a1b2c3d`. `names` prints the names, one a
# line, for the gate; with nothing, it only checks the file.
#
# `tests-failed <file>` names what failed in `cargo test --no-fail-fast`'s output instead: each
# test by its exact name, and a test binary that failed without naming one, by not building or by
# crashing, as that, which nothing known matches.
set -euo pipefail
if [ "${1:-}" = tests-failed ]; then
  said=$2
  sed -n 's/^---- \(.*\) stdout ----$/\1/p' "$said"
  # Every binary that failed says so once; one that named no test, crashed or didn't build.
  binaries=$(grep -c "^error: test failed, to rerun" "$said" || true)
  named=$(grep -c "^test result: FAILED" "$said" || true)
  if [ "$binaries" -gt "$named" ]; then
    echo "a test binary that failed without naming a test"
  fi
  if grep -q "^error: could not compile" "$said"; then
    echo "a test that didn't build"
  fi
  exit 0
fi
file="$(git rev-parse --show-toplevel)/.github/known-failures.txt"
failed=0
number=0
list=()
while IFS= read -r line || [ -n "$line" ]; do
  number=$((number + 1))
  case "$line" in "" | "#"*) continue ;; esac
  name=${line% | *}
  change=${line##* | }
  if [ "$name" = "$line" ] || [ -z "$name" ] || ! printf '%s' "$change" | grep -Eq '^(#[0-9]+|[0-9a-f]{7,40})$'; then
    echo "known-failures.txt:$number: \`$line\` isn't \`<exact name> | <#pull request or commit>\`" >&2
    failed=1
    continue
  fi
  list+=("$name")
done < "$file"
[ "$failed" -eq 0 ] || exit 1
if [ "${1:-}" = names ] && [ "${#list[@]}" -gt 0 ]; then
  printf '%s\n' "${list[@]}"
fi
