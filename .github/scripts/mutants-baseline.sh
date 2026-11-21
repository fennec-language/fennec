#!/usr/bin/env bash
# mutants-baseline.sh PACKAGE... | --diff FILE: runs the tests a mutant in these packages runs
# (`mutants-tests.sh`) on the code as it is, which have to pass, and prints a mutant's limit in
# seconds: five times what they took, and at least a minute. The build isn't counted.
#
# cargo-mutants' own baseline runs only the mutated package's tests, so its limit leaves out the
# end-to-end set. On 2026-11-20 that made fennec-rt's 40 seconds, and 92 of its 94 mutants timed
# out, some after a test had already failed. Mutants run two at a time, so the five is two runs at
# once with room to spare.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
{ read -r packages; read -r filter; } < <("$here/mutants-tests.sh" "$@")
chosen=()
IFS=, read -ra listed <<< "$packages"
for package in "${listed[@]}"; do
  chosen+=(--package "$package")
done
cargo nextest run --no-run "${chosen[@]}" >&2
start=$(date +%s)
cargo nextest run "${chosen[@]}" -E "$filter" >&2
took=$(($(date +%s) - start))
limit=$((5 * took))
echo $((limit > 60 ? limit : 60))
