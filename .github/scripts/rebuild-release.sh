#!/usr/bin/env bash
# Rebuilds a release from its tag, off GitHub, and checks that what it built is what the release's
# manifests name, byte for byte, before anyone signs them (docs/releases.md):
#
#   .github/scripts/rebuild-release.sh v0.4.2 fennec-v0.4.2.manifest fennec-launcher-v0.4.2.manifest
#
# It clones the tag afresh into a folder of its own and builds this machine's Linux archives in the
# containers the release was built in, pinned as the tag pins them, with the same scripts
# (`release-container.sh`, `release-env.sh`, `release-build.sh`). Each archive's line, its name, size
# and BLAKE3 hash as `fennec-dist manifest` writes it, has to be the manifest's own. It exits 0 only
# when every archive it built is the same, and says which aren't.
#
# It builds what this machine runs without emulation: Arm Linux on an Arm machine, x86-64 Linux on
# an x86-64 one. The rest rest on CI's `Built twice, the same` check. It needs git and Docker; set
# `REBUILD_DOCKER` to run Docker another way, like `docker --context colima-fennec`. The clone goes
# in `REBUILD_DIR`, by default `~/.fennec-rebuild`, which Docker on a Mac can see.
set -euo pipefail
tag=${1:?give the release tag, like v0.4.2}
toolchain_manifest=${2:?give the manifest of the toolchain, fennec-TAG.manifest}
launcher_manifest=${3:?and the manifest of the launcher, fennec-launcher-TAG.manifest}
repo=${REBUILD_REPO:-https://github.com/fennec-language/fennec.git}
read -r -a docker <<< "${REBUILD_DOCKER:-docker}"
for manifest in "$toolchain_manifest" "$launcher_manifest"; do
  [ -f "$manifest" ] || { echo "rebuild: there's no manifest $manifest" >&2; exit 2; }
done
toolchain_manifest=$(cd "$(dirname "$toolchain_manifest")" && pwd)/$(basename "$toolchain_manifest")
launcher_manifest=$(cd "$(dirname "$launcher_manifest")" && pwd)/$(basename "$launcher_manifest")

base=${REBUILD_DIR:-$HOME/.fennec-rebuild}
mkdir -p "$base"
work=$(mktemp -d "$base/$tag.XXXXXX")
echo "rebuild: cloning $tag from $repo into $work" >&2
git clone --quiet --depth 1 --branch "$tag" "$repo" "$work/fennec"
src="$work/fennec"
commit=$(git -C "$src" rev-parse HEAD)
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' "$src/Cargo.toml")
[ "v$version" = "$tag" ] || { echo "rebuild: $tag builds as version $version" >&2; exit 1; }

# The images the tag builds in, as its release workflow pins them.
workflow="$src/.github/workflows/release.yml"
ubuntu=$(grep -o 'ubuntu:24.04@sha256:[0-9a-f]\{64\}' "$workflow" | head -n 1)
alma=$(grep -o 'almalinux:8@sha256:[0-9a-f]\{64\}' "$workflow" | head -n 1)
[ -n "$ubuntu" ] && [ -n "$alma" ] || { echo "rebuild: $tag's release.yml pins no containers to build in" >&2; exit 1; }

arch=$("${docker[@]}" run --rm "$alma" uname -m)
target="$arch-unknown-linux-gnu"
run() {
  "${docker[@]}" run --rm -v "$work:$work" -w "$src" "$1" bash -c "$2"
}
echo "rebuild: $tag is $commit; building $target" >&2
# Expanded in the container, not here.
# shellcheck disable=SC2016
run "$ubuntu" 'set -e
.github/scripts/release-container.sh ubuntu
export PATH=$HOME/.cargo/bin:$PATH
source .github/scripts/release-env.sh "$(uname -m)-unknown-linux-musl"
.github/scripts/musl-runtime.sh musl'
run "$alma" "set -e
.github/scripts/release-container.sh alma
export PATH=\$HOME/.cargo/bin:\$PATH
.github/scripts/release-build.sh $target
cargo build --quiet --release --locked -p fennec-dist --target-dir target/dist
target/dist/release/fennec-dist manifest toolchain $version fennec-v$version-$target.tar.gz > rebuilt-toolchain.manifest
target/dist/release/fennec-dist manifest launcher $version fennec-launcher-v$version-$target.tar.gz > rebuilt-launcher.manifest
# The containers build as root: what they made is handed back, so it can be read and removed.
chown -R $(id -u):$(id -g) $work"

# Each archive's line, rebuilt, has to be a line of the manifest that's to be signed.
differ=0
for pair in "$src/rebuilt-toolchain.manifest $toolchain_manifest" "$src/rebuilt-launcher.manifest $launcher_manifest"; do
  read -r rebuilt manifest <<< "$pair"
  [ "$(head -n 1 "$rebuilt")" = "$(head -n 1 "$manifest")" ] ||
    { echo "rebuild: $manifest is for $(head -n 1 "$manifest"), not $(head -n 1 "$rebuilt")" >&2; exit 1; }
  while read -r line; do
    name=$(echo "$line" | awk '{print $2}')
    if grep -qxF -- "$line" "$manifest"; then
      echo "the same: $name"
    else
      differ=1
      echo "NOT THE SAME: $name"
      echo "  rebuilt:  $line"
      echo "  manifest: $(grep -F " $name " "$manifest" || echo "(no line for it)")"
    fi
  done < <(tail -n +2 "$rebuilt")
done
if [ "$differ" = 0 ]; then
  echo "rebuild: every archive built here from $tag ($commit) is the manifest's; the rest rest on CI's built-twice check"
  rm -rf "$work"
else
  echo "rebuild: don't sign: what $tag builds here isn't what the manifest names. The build is kept in $work" >&2
fi
exit "$differ"
