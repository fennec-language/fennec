#!/bin/sh
# Installs Fennec: curl -fsSL https://fennec-lang.dev/install.sh | sh
#
# It fetches the newest release for this machine, or the one FENNEC_VERSION names (like v0.1.0),
# checks it against the release's SHA256SUMS, and unpacks it into ~/.fennec/toolchains/VERSION.
# ~/.fennec/bin/fennec then runs that version. Nothing outside ~/.fennec is changed: it says what
# to add to PATH instead of editing a shell's profile.
#
# FENNEC_HOME puts it somewhere other than ~/.fennec. FENNEC_DOWNLOAD is where releases are
# fetched from, for a mirror or a test.
set -eu

repository=fennec-language/fennec
download=${FENNEC_DOWNLOAD:-https://github.com/$repository/releases/download}
home=${FENNEC_HOME:-$HOME/.fennec}

say() { printf 'fennec: %s\n' "$*" >&2; }
fail() { say "$*"; exit 1; }

fetch() {
  if command -v curl > /dev/null 2>&1; then
    curl -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget > /dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    fail "this needs curl or wget to download Fennec"
  fi
}

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  *) fail "there's no Fennec release for $(uname -s) on $(uname -m) yet" ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

version=${FENNEC_VERSION:-}
if [ -z "$version" ]; then
  # Releases are marked as pre-releases for now, which GitHub's "latest" leaves out, so this asks
  # for the newest of all.
  fetch "https://api.github.com/repos/$repository/releases?per_page=1" "$work/releases.json"
  version=$(sed -n 's/^ *"tag_name": *"\([^"]*\)".*/\1/p' "$work/releases.json" | head -n 1)
  [ -n "$version" ] || fail "couldn't find a Fennec release; set FENNEC_VERSION to the one you want"
fi

archive="fennec-$version-$target.tar.gz"
say "downloading Fennec $version for $target"
fetch "$download/$version/$archive" "$work/$archive" || fail "there's no Fennec $version for $target"
fetch "$download/$version/SHA256SUMS" "$work/SHA256SUMS" || fail "Fennec $version has no SHA256SUMS to check it against"

expected=$(awk -v name="$archive" '$2 == name || $2 == "*" name { print $1 }' "$work/SHA256SUMS")
[ -n "$expected" ] || fail "$archive isn't in the release's SHA256SUMS"
if command -v sha256sum > /dev/null 2>&1; then
  actual=$(sha256sum "$work/$archive" | awk '{ print $1 }')
else
  actual=$(shasum -a 256 "$work/$archive" | awk '{ print $1 }')
fi
[ "$actual" = "$expected" ] || fail "$archive doesn't match its checksum, so it wasn't installed"

tar -xzf "$work/$archive" -C "$work"
toolchain="$home/toolchains/$version"
rm -rf "$toolchain"
# Its user's alone, as fennec makes it: what's kept there is run. One that's another user's, or
# that anyone can write to, may hold what isn't this user's, so it's refused, as fennec refuses it.
if [ -e "$home" ]; then
  [ -n "$(find "$home" -prune -user "$(id -u)")" ] || fail "$home is another user's: set FENNEC_HOME to a folder of your own"
  [ -z "$(find "$home" -prune -perm -0002)" ] || fail "$home can be written by anyone, so what's in it may not be yours: \`chmod -R go-w $home\` fixes it, or remove it"
fi
(umask 077 && mkdir -p "$home/toolchains" "$home/bin")
chmod go-rwx "$home" "$home/toolchains"
mv "$work/fennec-$version-$target" "$toolchain"

# Not a link: `fennec` finds its runtime libraries and LLVM's tools beside itself, and on macOS
# it can see a link's path instead of the file's.
printf '#!/bin/sh\nexec "%s/fennec" "$@"\n' "$toolchain" > "$home/bin/fennec"
chmod +x "$home/bin/fennec"

say "installed Fennec $version in $toolchain"
case ":$PATH:" in
  *":$home/bin:"*) ;;
  *) say "add it to PATH, in your shell's profile: export PATH=\"$home/bin:\$PATH\"" ;;
esac
