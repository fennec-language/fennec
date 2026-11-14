#!/usr/bin/env bash
# Holds .github/tsan-suppressions.txt to its rules: each entry names one runtime function exactly,
# `race:fennec_rt::...` with no wildcard, so nothing in a program's own code is ever passed over,
# and has a `# why:` line just above it.
set -euo pipefail
file="$(git rev-parse --show-toplevel)/.github/tsan-suppressions.txt"
failed=0
previous=""
number=0
while IFS= read -r line || [ -n "$line" ]; do
  number=$((number + 1))
  case "$line" in
    "" | "#"*) ;;
    *)
      if ! printf '%s' "$line" | grep -Eq '^race:fennec_rt(::[A-Za-z_][A-Za-z0-9_]*)+$'; then
        echo "tsan-suppressions.txt:$number: \`$line\` isn't one runtime function by its exact name, \`race:fennec_rt::...\`" >&2
        failed=1
      fi
      case "$previous" in
        "# why: "?*) ;;
        *)
          echo "tsan-suppressions.txt:$number: \`$line\` has no \`# why:\` line above it" >&2
          failed=1
          ;;
      esac
      ;;
  esac
  previous=$line
done < "$file"
exit "$failed"
