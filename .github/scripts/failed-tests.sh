#!/usr/bin/env bash
# Each test that failed in a `cargo test` log, a line each, with where it panicked and what the
# panic said, so a failed run can be read in one line:
#
#   .github/scripts/failed-tests.sh tests.txt
#
# prints, for example,
#
#   cli::a_run_with_another_output_switch_never_gets_the_program_built_without_it: crates/fennec/tests/cli.rs:1653: assertion `left == right` failed: a second runs the one the store has
#
# A test binary that stopped without naming a test, killed by a signal or failing to build, is
# named by the `error:` line cargo gives it. Nothing printed means nothing failed.
set -u
log=$1
awk '
  # The test that panicked, where, and the line after: what the panic said.
  /^thread .* panicked at / {
    name = $0; sub(/^thread \047/, "", name); sub(/\047.*/, "", name)
    where = $0; sub(/.* panicked at /, "", where); sub(/:[0-9]+:$/, "", where)
    want = name; next
  }
  want != "" { said[want] = where ": " $0; want = ""; next }
  /^test .* \.\.\. FAILED$/ {
    name = $0; sub(/^test /, "", name); sub(/ \.\.\. FAILED$/, "", name)
    if (!(name in seen)) { seen[name] = 1; order[++n] = name }
  }
  # What cargo says of a test binary that failed, which names it when no test does.
  /^error: test failed, to rerun pass / { binaries[++b] = $0 }
  /process didn.t exit successfully: .*\(signal: / { binaries[++b] = $0 }
  END {
    for (i = 1; i <= n; i++) print order[i] (order[i] in said ? ": " said[order[i]] : "")
    if (n == 0) for (i = 1; i <= b; i++) print binaries[i]
  }
' "$log" | cut -c1-400
