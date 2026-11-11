#!/usr/bin/env bash
# The nightly run's builds of everything there is, checking what the optimizer is told. Every
# example and kit is built both ways and must print its `.out`; every kernel is built both ways and
# must finish. With `FENNEC_CHECK_PROOFS=1` set, as `proofs.yml` sets it, each check left out for a
# proof is an assertion, so a program stopping with "proof broken" has found a false one.
#
# Once `fennec` takes `FENNEC_STRIP_FACTS`, which its sources then read, each one is
# built again with every fact handed to LLVM stripped (`!range`, `noalias`, TBAA, `assume`), and
# must print what its normal build printed. A difference means a fact was wrong, which is the only
# check that catches a wrong TBAA tag.
#
#   .github/scripts/nightly-proofs.sh target/release/fennec report.md
#
# Each line of the report says what ran and what broke. It exits 1 if anything did. To try it on
# a few programs, `NIGHTLY_ONLY` names a pattern each one's path must match, as `case` does, and
# the floor on how many builds ran doesn't apply.
set -u
fennec=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
report=$2
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
within=.github/scripts/within.sh
strip=no
grep -rqs 'FENNEC_STRIP_FACTS' crates/llvm/src crates/fennec/src && strip=yes
broken=0
builds=0
stripped=0
said() {
  echo "$1" >> "$report"
  echo "$1"
}
# Fails the night, loudly, naming what and why.
broke() {
  echo "::error::$1"
  said "- BROKEN: $1"
  broken=1
}

# Builds `$1` (a program or a kit's folder) the way `$2` says, dev or release, to `$3`, with the
# extra environment `$4`, if any.
build() {
  local release=() extra=()
  [ "$2" = release ] && release=(--release)
  [ -n "$4" ] && extra=("$4")
  env ${extra[@]+"${extra[@]}"} "$within" 300 "$fennec" build ${release[@]+"${release[@]}"} "$1" -o "$3" > /dev/null 2> "$work/built.txt"
}

# Runs `$1` in folder `$2`, keeping what it printed in `$3` and what it said in `$3.said`, and
# gives back how it exited.
run() {
  local ran=0
  (cd "$2" && "$OLDPWD/$within" 300 "$1" < /dev/null > "$3" 2> "$3.said") || ran=$?
  return "$ran"
}

# Checks one program: built `$how`, it prints `$expected` (if given), never breaks a proof, and,
# with facts stripped, prints and exits as it did with them.
check() {
  local program=$1 folder=$2 expected=$3 how
  # shellcheck disable=SC2254
  case "$program" in ${NIGHTLY_ONLY:-*}) ;; *) return ;; esac
  for how in dev release; do
    if ! build "$program" "$how" "$work/normal" ""; then
      grep -qE "linking failed|failed on|couldn't run|panicked|didn't finish" "$work/built.txt" &&
        broke "$program didn't build in $how: $(head -c 300 "$work/built.txt")"
      continue
    fi
    builds=$((builds + 1))
    local ran=0
    run "$work/normal" "$folder" "$work/printed" || ran=$?
    if grep -q "proof broken" "$work/printed.said"; then
      broke "$program built in $how broke a proof: $(grep -m1 'proof broken' "$work/printed.said")"
    elif [ -n "$expected" ] && ! diff -q --strip-trailing-cr "$expected" "$work/printed" > /dev/null; then
      broke "$program built in $how didn't print its .out"
    elif [ -z "$expected" ] && [ "$ran" != 0 ]; then
      broke "$program built in $how exited $ran: $(head -c 300 "$work/printed.said")"
    fi
    [ "$strip" = yes ] || continue
    if ! build "$program" "$how" "$work/stripped" "FENNEC_STRIP_FACTS=1"; then
      broke "$program didn't build in $how with facts stripped: $(head -c 300 "$work/built.txt")"
      continue
    fi
    stripped=$((stripped + 1))
    local bare=0
    run "$work/stripped" "$folder" "$work/bare" || bare=$?
    if [ "$bare" != "$ran" ] || ! cmp -s "$work/printed" "$work/bare"; then
      broke "$program built in $how prints or exits otherwise with facts stripped ($ran, then $bare): a fact handed to LLVM is wrong"
    fi
  done
}

echo "## Every example, kit and kernel" >> "$report"
for program in examples/*.fen; do
  check "$program" . "${program%.fen}.out"
done
for kit in examples/projects/*/; do
  [ -f "$kit/kit.toml" ] || continue
  check "${kit%/}" . "${kit%/}.out"
done
for kernel in bench/kernels/*.fen; do
  check "$kernel" bench/kernels ""
done
said "- $builds builds ran with every proof checked, $stripped with facts stripped$([ "$strip" = yes ] || echo " (\`FENNEC_STRIP_FACTS\` isn't built yet)")."
[ -n "${NIGHTLY_ONLY:-}" ] || [ "$builds" -ge 100 ] || broke "only $builds builds ran"
exit "$broken"
