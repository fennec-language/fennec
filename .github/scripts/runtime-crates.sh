#!/usr/bin/env bash
# The crates from outside Fennec that the runtime library's own crates name, on every system and
# with every feature, against `.github/runtime-crates.txt`: a crate added to every Fennec program
# has to be listed there, with what it's for, in the pull request that adds it.
#
#   .github/scripts/runtime-crates.sh
set -euo pipefail
tree=$(mktemp)
trap 'rm -f "$tree"' EXIT
cargo tree --locked --offline -p fennec-runtime --features threads -e normal --prefix depth --target all > "$tree" \
  || cargo tree --locked -p fennec-runtime --features threads -e normal --prefix depth --target all > "$tree"
# Each crate under one of ours that isn't ours: `cargo tree` puts the depth before each name.
named=$(awk '{
  match($1, /^[0-9]+/); depth = substr($1, 1, RLENGTH); name = substr($1, RLENGTH + 1)
  at[depth] = name
  if (depth > 0 && at[depth - 1] ~ /^fennec/ && name !~ /^fennec/) print name
}' "$tree" | sort -u)
listed=$(sed -n 's/^\([a-z0-9_-]*\) |.*/\1/p' .github/runtime-crates.txt | sort -u)
failed=0
for crate in $(comm -23 <(echo "$named") <(echo "$listed")); do
  echo "::error::the runtime now names $crate, which every Fennec program would carry: add \`$crate | what it's for\` to .github/runtime-crates.txt, if it has to be there"
  failed=1
done
for crate in $(comm -13 <(echo "$named") <(echo "$listed")); do
  echo "::error::the runtime no longer names $crate: take it out of .github/runtime-crates.txt"
  failed=1
done
[ "$failed" -eq 0 ] && echo "The runtime names the $(echo "$named" | wc -l | tr -d ' ') crates .github/runtime-crates.txt lists"
exit "$failed"
