#!/usr/bin/env bash
# Tries `release-upload.sh` against a bucket in a folder, with an rclone and a fennec-dist that
# answer as theirs do: nothing goes up unless the release is signed, the manifest signed is the one
# built, the archives are its own and `latest` names nothing newer; `latest` goes up last; a file
# already up never changes; and `check` writes nothing.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() {
  echo "FAILED: $*"
  failures=$((failures + 1))
}

mkdir -p "$work/bin"
# An rclone whose `r2:` is the folder `$work/bucket`, as rclone does: `lsf` lists, `cat` reads,
# `copyto` writes, and with `--immutable` refuses to change a file that's there with other bytes.
# It fails as a wrong token does when `$work/denied` exists, and notes each write in `$work/wrote`.
cat > "$work/bin/rclone" << 'EOF'
#!/usr/bin/env bash
[ -e "$WORK/denied" ] && { echo "AccessDenied: 403" >&2; exit 1; }
[ "$RCLONE_CONFIG_R2_TYPE" = s3 ] && [ "$RCLONE_CONFIG_R2_ENDPOINT" = https://account.r2.cloudflarestorage.com ] || exit 9
immutable=0 include="" args=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --immutable) immutable=1 ;;
    --include) include=$2; shift ;;
    --config | --max-depth | --header-upload) shift ;;
    --*) ;;
    *) args+=("$1") ;;
  esac
  shift
done
command=${args[0]}
args=("${args[@]:1}")
at() { echo "$WORK/bucket/${1#r2:fennec-dl}"; }
case "$command" in
  lsf)
    if [ -n "$include" ]; then
      if [ -f "$(at "${args[0]}")$include" ]; then echo "${include#/}"; fi
    else
      ls "$(at "${args[0]}")"
    fi
    ;;
  cat) cat "$(at "${args[0]}")" ;;
  copyto)
    to=$(at "${args[1]}")
    if [ -f "$to" ] && ! cmp -s "${args[0]}" "$to"; then
      [ "$immutable" = 1 ] && { echo "Source and destination exist but do not match: immutable file modified" >&2; exit 1; }
    fi
    mkdir -p "$(dirname "$to")"
    cp "${args[0]}" "$to"
    echo "${args[1]#r2:fennec-dl/}" >> "$WORK/wrote"
    ;;
  *) exit 2 ;;
esac
EOF
# A fennec-dist whose manifest is each archive's name, size and hash, and whose `verify` passes a
# `.sig` that says it's signed by two keys, as the release keys' would.
cat > "$work/bin/fennec-dist" << 'EOF'
#!/usr/bin/env bash
case "$1" in
  manifest)
    echo "fennec $2 $3"
    shift 3
    for archive in "$@"; do
      echo "aarch64-apple-darwin $(basename "$archive") $(wc -c < "$archive" | tr -d ' ') $(shasum -a 256 "$archive" | cut -d ' ' -f 1)"
    done
    ;;
  verify)
    grep -q 'two of the release keys' "$4" || { echo "error: the launcher release is signed by 0 of the 0 keys this build of Fennec trusts, and it needs 2" >&2; exit 1; }
    echo "fennec launcher: signed as it needs to be"
    ;;
esac
EOF
chmod +x "$work/bin/rclone" "$work/bin/fennec-dist"

# A release of launcher `$1`: what this run built, and what the signer put on the release.
release() {
  rm -rf "$work/built" "$work/signed" "$work/wrote"
  mkdir -p "$work/built" "$work/signed"
  echo "launcher $1 for the Arm Mac" > "$work/built/fennec-launcher-v$1-aarch64-apple-darwin.tar.gz"
  echo "launcher $1 for Linux" > "$work/built/fennec-launcher-v$1-aarch64-unknown-linux-gnu.tar.gz"
  "$work/bin/fennec-dist" manifest launcher "$1" "$work"/built/*.tar.gz > "$work/built/fennec-launcher-v$1.manifest"
  cp "$work/built/fennec-launcher-v$1.manifest" "$work/signed/"
  echo "signed by two of the release keys" > "$work/signed/fennec-launcher-v$1.manifest.sig"
}
upload() {
  env PATH="$work/bin:$PATH" WORK="$work" R2_ACCOUNT_ID=account R2_ACCESS_KEY_ID=id R2_SECRET_ACCESS_KEY=secret \
    RELEASE_UPLOAD_RCLONE="$work/bin/rclone" RELEASE_UPLOAD_DIST="$work/bin/fennec-dist" \
    RELEASE_UPLOAD_PUBLIC="file://$work/bucket/launcher" \
    "$here/release-upload.sh" "$@" > "$work/said" 2>&1
}
# Nothing was written to the bucket.
untouched() {
  [ -f "$work/wrote" ] && fail "$1, yet it wrote $(paste -s -d ' ' - < "$work/wrote")"
  grep -q 'Nothing was uploaded' "$work/said" || fail "$1, and it didn't say nothing was uploaded: $(cat "$work/said")"
}
mkdir -p "$work/bucket"

# The token check lists the bucket, and writes nothing.
upload check || fail "the check failed on a token that lists the bucket: $(cat "$work/said")"
[ -f "$work/wrote" ] && fail "the check wrote to the bucket"
touch "$work/denied"
upload check && fail "the check passed on a token the bucket refused"
grep -q "can't list r2:fennec-dl" "$work/said" || fail "a refused token didn't say so: $(cat "$work/said")"
rm "$work/denied"
env -u R2_SECRET_ACCESS_KEY PATH="$work/bin:$PATH" RELEASE_UPLOAD_RCLONE="$work/bin/rclone" \
  "$here/release-upload.sh" check > /dev/null 2>&1 && fail "the check passed with no token"

# What isn't signed, rebuilt and its own never goes up.
release 1.0.0
rm "$work/signed/fennec-launcher-v1.0.0.manifest.sig"
upload upload 1.0.0 "$work/built" "$work/signed" && fail "an unsigned release was uploaded"
untouched "an unsigned release was refused"
grep -q "isn't signed: the release has no fennec-launcher-v1.0.0.manifest.sig" "$work/said" || fail "it didn't say the release isn't signed: $(cat "$work/said")"

release 1.0.0
echo "signed by one key" > "$work/signed/fennec-launcher-v1.0.0.manifest.sig"
upload upload 1.0.0 "$work/built" "$work/signed" && fail "a release the release keys didn't sign was uploaded"
untouched "a badly signed release was refused"
grep -q "isn't signed as it needs to be: error: the launcher release is signed by 0 of the 0 keys" "$work/said" || fail "it didn't say why the signatures fall short: $(cat "$work/said")"

release 1.0.0
echo "aarch64-apple-darwin fennec-launcher-v1.0.0-x86_64-pc-windows-gnullvm.zip 9 abc" >> "$work/signed/fennec-launcher-v1.0.0.manifest"
upload upload 1.0.0 "$work/built" "$work/signed" && fail "a manifest other than the one built was uploaded"
untouched "another manifest was refused"
grep -q "isn't the one this run built" "$work/said" || fail "it didn't say the manifests differ: $(cat "$work/said")"

release 1.0.0
echo "changed" >> "$work/built/fennec-launcher-v1.0.0-aarch64-apple-darwin.tar.gz"
upload upload 1.0.0 "$work/built" "$work/signed" && fail "an archive that isn't the manifest's was uploaded"
untouched "a changed archive was refused"

release 1.0.0
rm "$work/built/fennec-launcher-v1.0.0-aarch64-unknown-linux-gnu.tar.gz"
upload upload 1.0.0 "$work/built" "$work/signed" && fail "a release missing an archive was uploaded"
untouched "a missing archive was refused"

# A signed release goes up, `latest` last, and reads back.
release 1.0.0
upload upload 1.0.0 "$work/built" "$work/signed" || fail "a signed release wasn't uploaded: $(cat "$work/said")"
[ "$(tail -n 1 "$work/wrote")" = launcher/latest ] || fail "latest didn't go up last: $(paste -s -d ' ' - < "$work/wrote")"
[ "$(wc -l < "$work/wrote" | tr -d ' ')" = 5 ] || fail "it didn't upload two archives, the manifest, its .sig and latest: $(paste -s -d ' ' - < "$work/wrote")"
[ "$(cat "$work/bucket/launcher/latest")" = 1.0.0 ] || fail "latest isn't 1.0.0"
for file in fennec-launcher-v1.0.0-aarch64-apple-darwin.tar.gz fennec-launcher-v1.0.0.manifest fennec-launcher-v1.0.0.manifest.sig; do
  cmp -s "$work/bucket/launcher/v1.0.0/$file" "$( [ -f "$work/built/$file" ] && echo "$work/built/$file" || echo "$work/signed/$file")" || fail "$file in the bucket isn't what was signed"
done
grep -q 'serves launcher v1.0.0' "$work/said" || fail "it didn't read the release back: $(cat "$work/said")"

# The same again is fine; other bytes under a version already up aren't.
upload upload 1.0.0 "$work/built" "$work/signed" || fail "uploading the same release again failed: $(cat "$work/said")"
rm -f "$work/wrote"
echo "another build" > "$work/bucket/launcher/v1.0.0/fennec-launcher-v1.0.0-aarch64-apple-darwin.tar.gz"
upload upload 1.0.0 "$work/built" "$work/signed" && fail "a file already up was replaced with other bytes"
grep -q 'already holds other bytes' "$work/said" || fail "it didn't say a file up there differs: $(cat "$work/said")"
grep -qs launcher/latest "$work/wrote" && fail "latest moved after a file failed to go up"

# `latest` only moves on.
release 1.1.0
upload upload 1.1.0 "$work/built" "$work/signed" || fail "a newer release wasn't uploaded: $(cat "$work/said")"
[ "$(cat "$work/bucket/launcher/latest")" = 1.1.0 ] || fail "latest didn't move to 1.1.0"
release 1.0.1
upload upload 1.0.1 "$work/built" "$work/signed" && fail "an older release moved latest back"
untouched "an older release was refused"
grep -q 'latest launcher is 1.1.0, newer than 1.0.1' "$work/said" || fail "it didn't say latest is newer: $(cat "$work/said")"
release 1.10.0
upload upload 1.10.0 "$work/built" "$work/signed" || fail "1.10.0 was taken as older than 1.1.0: $(cat "$work/said")"

# A bucket that can't be read isn't taken for one without `latest`.
release 1.11.0
touch "$work/denied"
upload upload 1.11.0 "$work/built" "$work/signed" && fail "a bucket that couldn't be read was uploaded to"
rm "$work/denied"
untouched "an unreadable bucket was refused"
grep -q "couldn't read r2:fennec-dl" "$work/said" || fail "it didn't say the bucket couldn't be read: $(cat "$work/said")"

if [ "$failures" -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "release-upload.sh: every case as it should be"
