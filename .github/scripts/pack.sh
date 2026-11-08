#!/usr/bin/env bash
# Packs a release's folder into its archive the same way every time, so a release built twice is
# the same bytes: its files in order of their names, each dated `SOURCE_DATE_EPOCH`, owned by no one
# in particular, and with nothing of the machine's own, like macOS's file attributes.
#
#   SOURCE_DATE_EPOCH=1762550000 .github/scripts/pack.sh fennec-v0.4.2-aarch64-apple-darwin
#
# makes `fennec-v0.4.2-aarch64-apple-darwin.tar.gz` beside it, or `.zip` on Windows, and prints
# its name.
set -euo pipefail
folder=${1:?give the folder to pack}
epoch=${SOURCE_DATE_EPOCH:?set SOURCE_DATE_EPOCH, the time of the commit being released}
[ -d "$folder" ] || { echo "pack: there's no folder $folder" >&2; exit 1; }
# Every file and folder dated the same, deepest first, so dating a file doesn't change its folder's.
find "$folder" -depth -print0 | perl -0 -ne 'chomp; utime($ENV{SOURCE_DATE_EPOCH}, $ENV{SOURCE_DATE_EPOCH}, $_) or die "pack: couldn'"'"'t date $_: $!\n"'
case "$(uname -s)" in
  Linux)
    tar --sort=name --format=gnu --owner=0 --group=0 --numeric-owner --mtime="@$epoch" -cf - "$folder" | gzip -9 -n > "$folder.tar.gz"
    echo "$folder.tar.gz"
    ;;
  Darwin)
    # macOS's tar has no sorting of its own, so it's given the names in order.
    LC_ALL=C find "$folder" | LC_ALL=C sort \
      | COPYFILE_DISABLE=1 tar --format=ustar --uid 0 --gid 0 --uname root --gname root --no-mac-metadata --no-xattrs -n -cf - -T - \
      | gzip -9 -n > "$folder.tar.gz"
    echo "$folder.tar.gz"
    ;;
  *)
    7z a -tzip "$folder.zip" "$folder" > /dev/null
    echo "$folder.zip"
    ;;
esac
