#!/usr/bin/env bash
# Puts a signed launcher release in the bucket `fennec self update` reads, `fennec-dl`, served at
# https://dl.fennec-lang.dev (docs/releases.md). The release workflow runs it, in the `release`
# environment, when it's started by hand on a tag that's been signed:
#
#   release-upload.sh install               fetches rclone, pinned by version and hash
#   release-upload.sh check                 lists the bucket, writing nothing, so the token is proved
#                                           before anything is published
#   release-upload.sh upload VERSION BUILT SIGNED
#
# `upload` takes the launcher archives and manifest this run built from the tag in BUILT, and the
# manifest and `.sig` the signer put on the GitHub release in SIGNED. It uploads nothing, and fails
# saying why, unless:
#   - the release has a `.sig`, and its signatures are the ones the launcher needs, from the release
#     keys built into `fennec-dist`: with none built in yet, nothing is ever uploaded;
#   - the manifest signed is the manifest this run built, byte for byte, and each archive is the
#     manifest's, so what's uploaded is what was rebuilt and signed;
#   - the bucket's `latest` names no newer launcher.
# Then it uploads the archives, the manifest and its `.sig` under `launcher/v<VERSION>/`, never
# replacing a file there with other bytes, and `latest` last, so `fennec self update` never sees a
# version whose files aren't all there. It reads `latest` and the manifest back from the public
# address.
#
# The bucket's token is R2_ACCOUNT_ID, R2_ACCESS_KEY_ID and R2_SECRET_ACCESS_KEY. RELEASE_UPLOAD_RCLONE
# and RELEASE_UPLOAD_DIST name the rclone and fennec-dist to run, RELEASE_UPLOAD_BUCKET the bucket
# (`r2:fennec-dl`) and RELEASE_UPLOAD_PUBLIC its public address for the launcher.
set -euo pipefail

rclone_version=v1.76.0
rclone_sha256=b00f00b3266c3764413afe599d3ec316f6f43b3e0ea8a0bc1d186d2133e46df7
rclone=${RELEASE_UPLOAD_RCLONE:-${RUNNER_TEMP:-/tmp}/rclone/rclone}
dist=${RELEASE_UPLOAD_DIST:-target/release/fennec-dist}
bucket=${RELEASE_UPLOAD_BUCKET:-r2:fennec-dl}
public=${RELEASE_UPLOAD_PUBLIC:-https://dl.fennec-lang.dev/launcher}

refuse() {
  echo "::error::release-upload: $*. Nothing was uploaded." >&2
  exit 1
}

# rclone reaches R2 as S3, configured here rather than in a file. The token is limited to the one
# bucket, so rclone mustn't try to make it.
r2() {
  : "${R2_ACCOUNT_ID:?set the release environment secrets}" "${R2_ACCESS_KEY_ID:?}" "${R2_SECRET_ACCESS_KEY:?}"
  env RCLONE_CONFIG_R2_TYPE=s3 RCLONE_CONFIG_R2_PROVIDER=Cloudflare \
    RCLONE_CONFIG_R2_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID" RCLONE_CONFIG_R2_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY" \
    RCLONE_CONFIG_R2_ENDPOINT="https://$R2_ACCOUNT_ID.r2.cloudflarestorage.com" RCLONE_CONFIG_R2_NO_CHECK_BUCKET=true \
    "$rclone" --config /dev/null "$@"
}

case "${1:-}" in
  install)
    folder=$(dirname "$rclone")
    zip="rclone-$rclone_version-linux-amd64.zip"
    mkdir -p "$folder"
    curl -fsSL --retry 3 -o "$folder/$zip" "https://github.com/rclone/rclone/releases/download/$rclone_version/$zip"
    echo "$rclone_sha256  $folder/$zip" | sha256sum -c --quiet - ||
      refuse "$zip isn't the rclone pinned here: its SHA-256 isn't $rclone_sha256"
    unzip -q -j -o "$folder/$zip" "rclone-$rclone_version-linux-amd64/rclone" -d "$folder"
    "$rclone" version | head -n 1
    ;;

  check)
    # Listing writes nothing, and a token that's wrong, expired or for another bucket can't.
    if ! listed=$(r2 lsf --max-depth 1 "$bucket" 2>&1); then
      refuse "the release environment's token can't list $bucket, so it can't upload either: $listed"
    fi
    echo "release-upload: the token lists $bucket: $(echo "$listed" | grep -c . || true) entries at its top"
    ;;

  upload)
    version=${2:?give the version, like 1.0.0}
    built=${3:?and the folder of what this run built}
    signed=${4:?and the folder of what the signer put on the release}
    manifest="fennec-launcher-v$version.manifest"

    [ -f "$built/$manifest" ] || refuse "this run built no $manifest"
    [ -f "$signed/$manifest.sig" ] ||
      refuse "launcher v$version isn't signed: the release has no $manifest.sig. It's uploaded only once it's been rebuilt and signed (docs/releases.md)"
    [ -f "$signed/$manifest" ] || refuse "the release has a $manifest.sig but no $manifest"
    cmp -s "$built/$manifest" "$signed/$manifest" ||
      refuse "the manifest signed isn't the one this run built from the tag: $(diff "$signed/$manifest" "$built/$manifest" | head -n 10 | paste -s -d ' ' -)"
    if ! verdict=$("$dist" verify launcher "$signed/$manifest" "$signed/$manifest.sig" 2>&1); then
      refuse "launcher v$version isn't signed as it needs to be: $verdict"
    fi
    echo "release-upload: $verdict"

    # The archives the manifest names, each the manifest's own size and hash.
    archives=()
    while read -r _ name _; do
      [ -f "$built/$name" ] || refuse "the manifest names $name, which this run didn't build"
      archives+=("$built/$name")
    done < <(tail -n +2 "$signed/$manifest")
    [ "${#archives[@]}" -gt 0 ] || refuse "the manifest names no archives"
    "$dist" manifest launcher "$version" "${archives[@]}" | cmp -s - "$signed/$manifest" ||
      refuse "the archives aren't the ones the manifest names"

    # Never back: `latest` only moves to a newer launcher.
    # A bucket that can't be read stops it, rather than read as one with no `latest`.
    if ! there=$(r2 lsf --files-only --max-depth 2 --include /launcher/latest "$bucket" 2>&1); then
      refuse "couldn't read $bucket: $there"
    fi
    live=""
    if [ -n "$there" ]; then
      live=$(r2 cat "$bucket/launcher/latest" | tr -d '[:space:]') || refuse "couldn't read the bucket's latest"
    fi
    if [ -n "$live" ] && [ "$live" != "$version" ] &&
      [ "$(printf '%s\n%s\n' "$live" "$version" | sort -V | tail -n 1)" = "$live" ]; then
      refuse "the bucket's latest launcher is $live, newer than $version"
    fi

    # The version's files never change once they're up, so they're cached for good, and a file
    # already there with other bytes stops the upload.
    for file in "${archives[@]}" "$signed/$manifest" "$signed/$manifest.sig"; do
      r2 copyto --immutable --header-upload "Cache-Control: public, max-age=31536000, immutable" \
        "$file" "$bucket/launcher/v$version/$(basename "$file")" ||
        refuse "$(basename "$file") couldn't be uploaded, or launcher/v$version/ already holds other bytes by that name. What went up before it is in the bucket, but latest still names ${live:-nothing}"
      echo "release-upload: uploaded launcher/v$version/$(basename "$file")"
    done
    latest=$(mktemp)
    echo "$version" > "$latest"
    r2 copyto --header-upload "Cache-Control: no-cache" "$latest" "$bucket/launcher/latest" ||
      refuse "launcher/v$version/ is up, but latest couldn't be moved to it"
    echo "release-upload: latest is $version"

    # Read back as a person's launcher reads them, past any cache.
    fresh=""
    case "$public" in http*) fresh="?uploaded=$(date +%s)" ;; esac
    [ "$(curl -fsSL --retry 3 "$public/latest$fresh" | tr -d '[:space:]')" = "$version" ] ||
      { echo "::error::release-upload: $public/latest doesn't read $version" >&2; exit 1; }
    curl -fsSL --retry 3 "$public/v$version/$manifest$fresh" | cmp -s - "$signed/$manifest" ||
      { echo "::error::release-upload: $public/v$version/$manifest isn't the manifest signed" >&2; exit 1; }
    echo "release-upload: $public serves launcher v$version"
    ;;

  *)
    echo "usage: release-upload.sh install | check | upload VERSION BUILT SIGNED" >&2
    exit 2
    ;;
esac
