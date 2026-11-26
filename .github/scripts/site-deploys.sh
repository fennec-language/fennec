#!/usr/bin/env bash
# Which of the two Workers a deploy puts up: what's built, as `site-hash.sh` wrote it, against the
# live site's `/build-hash.txt`. The site goes up most days with nothing changed in it, as any
# change to the compiler's crates rebuilds the language page, so it's put up only when its hash or
# the redirect's differs: the live file holds both, and the redirect's line is right only once the
# site that serves it is up too. The redirect goes up only when its own hash differs.
#
#   .github/scripts/site-deploys.sh site/dist/build-hash.txt live-build-hash.txt >> "$GITHUB_OUTPUT"
#
# prints `site=deploy` or `site=same`, and the same for `redirect`, and says why on standard
# error. A live file that's missing, empty or unreadable, which is how `deploy.yml` passes on a
# failed fetch or a run by hand, puts both up.
set -euo pipefail
built=${1:?give the built build-hash.txt}
live=${2:?and the live one, which may be empty}
line() {
  sed -n "s/^$1 \([0-9a-f]\{64\}\)$/\1/p" "$2" 2> /dev/null | head -n 1
}
for worker in site redirect; do
  [ -n "$(line "$worker" "$built")" ] || { echo "site-deploys: $built has no $worker line" >&2; exit 1; }
done
if [ -z "$(line site "$live")" ] || [ -z "$(line redirect "$live")" ]; then
  echo "site-deploys: there's no live hash to compare with, so both are deployed" >&2
  echo "site=deploy"
  echo "redirect=deploy"
  exit 0
fi
redirect=same
if [ "$(line redirect "$built")" != "$(line redirect "$live")" ]; then
  redirect=deploy
fi
site=same
if [ "$(line site "$built")" != "$(line site "$live")" ] || [ "$redirect" = deploy ]; then
  site=deploy
fi
said() {
  if [ "$2" = same ]; then echo "the $1 is the same as live"; else echo "the $1 goes up"; fi
}
echo "site-deploys: $(said site "$site"), and $(said redirect "$redirect")" >&2
echo "site=$site"
echo "redirect=$redirect"
