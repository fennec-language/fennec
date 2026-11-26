#!/usr/bin/env bash
# Tries `site-hash.sh` on a throwaway site and redirect, and `site-deploys.sh` against live hashes:
# the same files give the same hash, a change to either Worker's files changes its own hash only,
# and only what differs from what's live goes up, the site too when the redirect does, and both
# when there's no live hash to compare with.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() {
  echo "FAILED: $*"
  failures=$((failures + 1))
}

# A repository with only what the hash reads, and the script where it looks for it.
repo="$work/repo"
mkdir -p "$repo/.github/scripts" "$repo/site/dist/learn" "$repo/cloudflare/redirect/src"
cp "$here/site-hash.sh" "$repo/.github/scripts/"
echo '<h1>Fennec</h1>' > "$repo/site/dist/index.html"
echo '<h1>Learn</h1>' > "$repo/site/dist/learn/index.html"
echo '{"name": "fennec-site"}' > "$repo/site/wrangler.jsonc"
echo 'export default {}' > "$repo/cloudflare/redirect/src/index.js"
echo '{"name": "fennec-redirect"}' > "$repo/cloudflare/redirect/wrangler.jsonc"
hash() {
  "$repo/.github/scripts/site-hash.sh" > "$work/$1" || fail "site-hash.sh failed"
}
value() {
  sed -n "s/^$1 //p" "$work/$2"
}

hash first
grep -qE '^site [0-9a-f]{64}$' "$work/first" || fail "no site line: $(cat "$work/first")"
grep -qE '^redirect [0-9a-f]{64}$' "$work/first" || fail "no redirect line: $(cat "$work/first")"
# Written into the site, the file doesn't change its hash.
cp "$work/first" "$repo/site/dist/build-hash.txt"
hash again
cmp -s "$work/first" "$work/again" || fail "writing build-hash.txt into the site changed its hash"

echo '<h1>Learn more</h1>' > "$repo/site/dist/learn/index.html"
hash page
[ "$(value site page)" != "$(value site first)" ] || fail "a page's change didn't change the site's hash"
[ "$(value redirect page)" = "$(value redirect first)" ] || fail "a page's change changed the redirect's hash"
echo '<h1>Learn</h1>' > "$repo/site/dist/learn/index.html"

mv "$repo/site/dist/learn/index.html" "$repo/site/dist/learn/start.html"
hash moved
[ "$(value site moved)" != "$(value site first)" ] || fail "a page moved to another path didn't change the site's hash"
mv "$repo/site/dist/learn/start.html" "$repo/site/dist/learn/index.html"

echo '{"name": "fennec-site", "routes": []}' > "$repo/site/wrangler.jsonc"
hash config
[ "$(value site config)" != "$(value site first)" ] || fail "the site's Wrangler config didn't count"
echo '{"name": "fennec-site"}' > "$repo/site/wrangler.jsonc"

echo 'export default { fetch() {} }' > "$repo/cloudflare/redirect/src/index.js"
hash redirect
[ "$(value redirect redirect)" != "$(value redirect first)" ] || fail "the redirect's change didn't change its hash"
[ "$(value site redirect)" = "$(value site first)" ] || fail "the redirect's change changed the site's hash"

hash back
[ "$(value site back)" = "$(value site first)" ] || fail "the same site's files gave another hash"

# Which go up, from what's built and what's live, `site=… redirect=…` on one line.
deploys() {
  "$here/site-deploys.sh" "$1" "$2" 2> "$work/said" | paste -s -d ' ' -
}
lines() {
  printf 'site %s\nredirect %s\n' "$(printf '%064d' "$1")" "$(printf '%064d' "$2")" > "$work/$3"
}
lines 1 2 built
lines 1 2 live
[ "$(deploys "$work/built" "$work/live")" = "site=same redirect=same" ] || fail "the same as live went up: $(cat "$work/said")"
grep -q 'the site is the same as live' "$work/said" || fail "it didn't say why: $(cat "$work/said")"
lines 3 2 live
[ "$(deploys "$work/built" "$work/live")" = "site=deploy redirect=same" ] || fail "a changed site didn't go up alone"
lines 1 3 live
[ "$(deploys "$work/built" "$work/live")" = "site=deploy redirect=deploy" ] || fail "a changed redirect didn't take the site, which holds its hash, with it"
: > "$work/empty"
[ "$(deploys "$work/built" "$work/empty")" = "site=deploy redirect=deploy" ] || fail "no live hash didn't put both up"
grep -q 'no live hash' "$work/said" || fail "it didn't say there was no live hash: $(cat "$work/said")"
[ "$(deploys "$work/built" "$work/missing")" = "site=deploy redirect=deploy" ] || fail "a missing live file didn't put both up"
echo '<html>Not found</html>' > "$work/page"
[ "$(deploys "$work/built" "$work/page")" = "site=deploy redirect=deploy" ] || fail "a page that isn't the file didn't put both up"
"$here/site-deploys.sh" "$work/empty" "$work/live" > /dev/null 2>&1 && fail "a built file with no hashes was taken"

if [ "$failures" -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "site-hash.sh and site-deploys.sh: every case as it should be"
