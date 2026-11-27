#!/usr/bin/env bash
# Writes `.github/release-debs.lock`: every Ubuntu package the musl runtime's container installs
# (`release-container.sh ubuntu`), for x86-64 and Arm, by version and SHA-256, and where Launchpad
# keeps it. Ubuntu's archive drops a version once a newer one comes out, which failed a release run
# ("Version '2.42-4ubuntu2.10' for 'binutils' was not found"); Launchpad keeps every one. A bump is
# this one command, from the checkout, and a review of the lock's diff:
#
#   .github/scripts/release-debs.sh
#
# It resolves what's to be installed in the image release.yml pins, as apt would install it there
# today, and takes each file's hash from Launchpad, then downloads the file to check it. Anything
# missing or different stops it, and the lock is left as it was. It needs Docker, able to run both
# arches' images, curl and jq; set `RELEASE_DEBS_DOCKER` to run Docker another way, like
# `docker --context colima-fennec`.
set -euo pipefail
# What the runtime's built with, and what fetches and checks it out: git for the checkout, and
# curl, with the certificates it checks a site by, for rustup.
packages="ca-certificates curl git musl-tools gcc gcc-13 cpp-13 binutils libc6-dev"
read -r -a docker <<< "${RELEASE_DEBS_DOCKER:-docker}"
lock=.github/release-debs.lock
api=https://api.launchpad.net/devel
image=$(grep -o 'ubuntu:24.04@sha256:[0-9a-f]\{64\}' .github/workflows/release.yml | head -n 1)
[ -n "$image" ] || { echo "release-debs: release.yml pins no ubuntu image" >&2; exit 1; }
work=$(mktemp -d)
trap '/bin/rm -rf "$work"' EXIT

# Gets a URL, trying three times, and fails loudly when it can't.
fetch() {
  curl -sSfL --retry 3 --retry-all-errors "$@"
}

{
  echo "# Every package the musl runtime's container installs, by .github/scripts/release-container.sh"
  echo "# ubuntu: arch, name, version, SHA-256 and where Launchpad keeps it. Written by"
  echo "# .github/scripts/release-debs.sh, which a bump runs; not edited by hand."
  echo "image $image"
  echo "packages $packages"
} > "$work/lock"
for arch in amd64 arm64; do
  # Expanded in the container, not here.
  # shellcheck disable=SC2016
  "${docker[@]}" run --rm -i --platform "linux/$arch" "$image" bash -s "$packages" > "$work/$arch.inst" << 'EOF'
set -euo pipefail
apt-get update -qq > /dev/null
# shellcheck disable=SC2086
apt-get install -s --no-install-recommends $1 | grep '^Inst '
EOF
  # `Inst libc6 [2.39-0ubuntu8.9] (2.39-0ubuntu8.12 Ubuntu:24.04/noble-updates [arm64])`: what's
  # installed, at what version, for which arch, `all` for one file for both.
  sed -n 's/^Inst \([^ ]*\) .*(\([^ ]*\) .* \[\([^]]*\)\])$/\1 \2 \3/p' "$work/$arch.inst" > "$work/$arch.want"
  if [ "$(wc -l < "$work/$arch.want")" -ne "$(wc -l < "$work/$arch.inst")" ] || [ ! -s "$work/$arch.want" ]; then
    echo "release-debs: couldn't read what apt would install for $arch:" >&2
    cat "$work/$arch.inst" >&2
    exit 1
  fi
  while read -r name version file_arch; do
    # Launchpad's file is named without the version's epoch, as `git_2.43.0-1ubuntu7.4_arm64.deb`.
    file="${name}_${version#*:}_$file_arch.deb"
    published=$(fetch -G "$api/ubuntu/+archive/primary" --data-urlencode ws.op=getPublishedBinaries \
      --data-urlencode "binary_name=$name" --data-urlencode "version=$version" --data-urlencode exact_match=true \
      --data-urlencode "distro_arch_series=$api/ubuntu/noble/$arch" | jq -r '.entries[].self_link')
    [ -n "$published" ] || { echo "release-debs: Launchpad has no $name $version for $arch" >&2; exit 1; }
    # Published to more than one pocket, as updates and security, it's one file.
    sha=$(for link in $published; do
      fetch "$link?ws.op=binaryFileUrls&include_meta=true" | jq -r --arg file "$file" '.[] | select(.url | endswith("/" + $file)) | .sha256'
    done | sort -u)
    [ "$(printf '%s\n' "$sha" | grep -c '^[0-9a-f]\{64\}$')" -eq 1 ] \
      || { echo "release-debs: Launchpad gives not one SHA-256 for $file, but: ${sha:-none}" >&2; exit 1; }
    # Where Launchpad's link leads: its file server, which answers over plain HTTP, since the image
    # has no certificates to check a site by until this installs them. The hash is what's trusted.
    url=$(curl -sS --retry 3 -o /dev/null -w '%{redirect_url}' "https://launchpad.net/ubuntu/+archive/primary/+files/$file")
    url="http://${url#https://}"
    case "$url" in http://launchpadlibrarian.net/*/"$file") ;; *)
      echo "release-debs: Launchpad's link for $file leads to $url" >&2
      exit 1
      ;;
    esac
    [ -f "$work/$file" ] || fetch -o "$work/$file" "$url"
    got=$(shasum -a 256 "$work/$file" | cut -d ' ' -f 1)
    [ "$got" = "$sha" ] || { echo "release-debs: $url is $got, not the $sha Launchpad gives" >&2; exit 1; }
    echo "$arch $name $version $sha $url"
  done < "$work/$arch.want" | sort >> "$work/lock"
done
/bin/cp -f "$work/lock" "$lock"
echo "release-debs: wrote $lock: $(grep -c '^amd64 ' "$lock") packages for amd64, $(grep -c '^arm64 ' "$lock") for arm64" >&2
