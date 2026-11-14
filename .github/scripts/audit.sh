#!/usr/bin/env bash
# Runs every example and kit built for a heap audit and under it (`FENNEC_AUDIT` in
# docs/environment.md): each has to print what its `.out` says, with the audit finding nothing
# wrong in its heap as it ends. One at a time, each within its limit.
#
#   .github/scripts/audit.sh [names]
#
# It needs `fennec` and the audited runtime libraries beside it, as the gate builds them.
#
# First it checks the check itself: a program the runtime is told to leave a block in has to be
# told so, and end with 87. An audit that says nothing because it does nothing fails here, where
# every program would otherwise pass it.
#
# A program that fails is said, with what it wrote, and named `audit:<its path>` on a line of the
# file `names`, when one is given: the gate reads those against the failures main is known to have.
set -u
names=${1:-/dev/null}
fennec=./target/debug/fennec
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
printed="$work/printed.txt"
said="$work/said.txt"
# A build is audited by this in the builder's own environment, which a daemon started before now
# wouldn't have.
export FENNEC_AUDIT=1 FENNEC_NO_DAEMON=1

printf 'def main(stdout: Stdout):\n    stdout.print("ran")\n    return\n' > "$work/selftest.fen"
status=0
FENNEC_AUDIT_SELFTEST=1 .github/scripts/within.sh 120 "$fennec" run "$work/selftest.fen" \
  > "$printed" 2> "$said" || status=$?
if [ "$status" -ne 87 ] || ! grep -q '^heap audit: leaked: str ' "$said"; then
  echo "audit: a program made to leave a block wasn't told so; it ended with $status, saying:" >&2
  cat "$said" >&2
  echo "audit:selftest" >> "$names"
  exit 1
fi

failed=0
# Runs the program or kit `$1`, which has to print what the file `$2` holds.
audited() {
  if ! { .github/scripts/within.sh 120 "$fennec" run "$1" > "$printed" 2> "$said" \
    && diff --strip-trailing-cr "$2" "$printed" > /dev/null; }; then
    echo "audit: $1 failed under the heap audit, saying:" >&2
    cat "$said" >&2
    echo "audit:$1" >> "$names"
    failed=1
  fi
}
for program in examples/*.fen; do
  audited "$program" "${program%.fen}.out"
done
for kit in examples/projects/*/; do
  [ -f "$kit/kit.toml" ] || continue
  audited "${kit%/}" "${kit%/}.out"
done
exit "$failed"
