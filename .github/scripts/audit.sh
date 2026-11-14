#!/usr/bin/env bash
# Runs every example and kit built for a heap audit and under it (`FENNEC_AUDIT` in
# docs/environment.md): each has to print what its `.out` says, with the audit finding nothing
# wrong in its heap as it ends. One at a time, each within its limit.
#
#   .github/scripts/audit.sh [report.md]
#
# CI runs it on main every four hours, and the nightly run does too. The gate doesn't: it takes
# longer than the gate's other steps over the examples together, and every gate already checks
# that each example ends with no block live.
#
# `FENNEC` names the `fennec` to use, `./target/debug/fennec` unless given. The runtime libraries
# built with the audit have to be beside it, `libfennec_runtime_audit.a` and
# `libfennec_runtime_threads_audit.a`, or each build is refused.
#
# First it checks the check itself: a program the runtime is told to leave a block in has to be
# told so, and end with 87. An audit that says nothing because it does nothing fails here, where
# every program would otherwise pass it.
#
# A program that fails is said with what it wrote, which names each block the audit found wrong:
# its kind, its count and what holds it. With a report named, each is added to it the same way,
# and on GitHub it's an error on the run as well. It exits 1 if anything failed.
set -u
report=${1:-/dev/null}
fennec=${FENNEC:-./target/debug/fennec}
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
printed="$work/printed.txt"
said="$work/said.txt"
# A build is audited by this in the builder's own environment, which a daemon started before now
# wouldn't have.
export FENNEC_AUDIT=1 FENNEC_NO_DAEMON=1

# Says that `$1` failed, with what it wrote: here, in the report, and on GitHub's run.
broke() {
  local found label
  found=$(grep -m 1 -E '^heap audit: [a-z ]+: ' "$said" || true)
  found=${found#heap audit: }
  label="the heap audit of \`$1\`"
  if [ -n "$found" ]; then
    label="$label: $found"
  fi
  echo "audit: $1 failed under the heap audit, saying:" >&2
  cat "$said" >&2
  {
    echo "- BROKEN: $label"
    echo
    echo '  ```'
    if grep -q '^heap audit' "$said"; then
      grep '^heap audit' "$said" | sed 's/^/  /'
    else
      # Not a finding of the audit's: it didn't build, or printed something else.
      tail -n 20 "$said" | sed 's/^/  /'
    fi
    echo '  ```'
  } >> "$report"
  if [ -n "${GITHUB_ACTIONS:-}" ]; then
    if [ -z "$found" ]; then
      found="it failed under the heap audit, with no finding of the audit's"
    fi
    echo "::error title=Heap audit::$1: $found"
  fi
}

printf 'def main(stdout: Stdout):\n    stdout.print("ran")\n    return\n' > "$work/selftest.fen"
status=0
FENNEC_AUDIT_SELFTEST=1 .github/scripts/within.sh 120 "$fennec" run "$work/selftest.fen" \
  > "$printed" 2> "$said" || status=$?
if [ "$status" -ne 87 ] || ! grep -q '^heap audit: leaked: str ' "$said"; then
  {
    echo "a block left on purpose wasn't found, and the run ended with $status, saying:"
    cat "$said"
  } > "$work/why.txt"
  cat "$work/why.txt" > "$said"
  broke "the audit's own check"
  exit 1
fi

failed=0
ran=0
# Runs the program or kit `$1`, which has to print what the file `$2` holds.
audited() {
  ran=$((ran + 1))
  if ! { .github/scripts/within.sh 120 "$fennec" run "$1" > "$printed" 2> "$said" \
    && diff --strip-trailing-cr "$2" "$printed" > /dev/null; }; then
    broke "$1"
    failed=$((failed + 1))
  fi
}
for program in examples/*.fen; do
  audited "$program" "${program%.fen}.out"
done
for kit in examples/projects/*/; do
  [ -f "$kit/kit.toml" ] || continue
  audited "${kit%/}" "${kit%/}.out"
done
echo "- The heap audit ran $ran examples and kits, and $failed failed." | tee -a "$report"
# So a run that found no programs can't pass for one that found nothing wrong.
if [ "$ran" -lt 100 ]; then
  echo "audit: only $ran programs ran" >&2
  exit 1
fi
[ "$failed" -eq 0 ]
