#!/usr/bin/env bash
# Takes this worktree's own crates out of its builds, leaving the crates they use from outside, so
# the next build makes them again from nothing. For a corrupt build: objects cargo left half
# written when it was stopped mid-build, which link with undefined `….llvm.<number>` symbols until
# they're made again. The gate names one when it sees it (`known-failures.sh corrupt-build`).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
packages=()
while IFS= read -r name; do
  packages+=(-p "$name")
done < <(cargo metadata --locked --no-deps --format-version 1 | jq -r '.packages[].name')
# `target`, and each target folder of cargo's beside the usual one in it, as the gate makes for the
# threads runtime and for Windows: each has the tag cargo puts in a folder it builds in.
for folder in target target/*/; do
  folder=${folder%/}
  [ -f "$folder/CACHEDIR.TAG" ] || continue
  cargo clean --locked --target-dir "$folder" "${packages[@]}"
done
