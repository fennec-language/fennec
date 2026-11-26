#!/usr/bin/env bash
# What a deploy would put up, as a hash for each of the two Workers: the website, its built files
# in `site/dist` and its Wrangler config, and the fennec-lang.com redirect, its source and config.
# Each is a hash of every file's path and contents, so the same files give the same line wherever
# they were built:
#
#   .github/scripts/site-hash.sh > site/dist/build-hash.txt
#
# prints `site <hash>` and `redirect <hash>`. The site serves the file as `/build-hash.txt`, and
# `deploy.yml` deploys only what differs from the live one (`site-deploys.sh`). The file itself
# isn't counted, so writing it doesn't change the site's hash.
set -euo pipefail
cd "$(dirname "$0")/../.."
[ -d site/dist ] || { echo "site-hash: there's no site/dist; build the site first" >&2; exit 1; }
if command -v sha256sum > /dev/null; then
  sha() { sha256sum; }
else
  sha() { shasum -a 256; }
fi
# Every file under the paths given, in order of its path, with its own hash, hashed together.
files() {
  find "$@" -type f ! -path site/dist/build-hash.txt -print0 | LC_ALL=C sort -z \
    | while IFS= read -r -d '' file; do printf '%s %s\n' "$(sha < "$file" | cut -d' ' -f1)" "$file"; done \
    | sha | cut -d' ' -f1
}
echo "site $(files site/dist site/wrangler.jsonc)"
echo "redirect $(files cloudflare/redirect)"
