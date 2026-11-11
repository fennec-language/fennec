#!/usr/bin/env bash
# The flake ledger. A check that fails and then passes on the same code is a flake, and every one
# is written down, so nothing is ever "just rerun": the coverage lane reads the ledger nightly, and
# `fennec-flakes` lists it by test.
#
#   flakes.sh failed STEP LOG    a gate step failed, printing LOG: it's kept, pending, under the
#                                code it tested
#   flakes.sh passed             the gate passed: what failed before on the same code goes in the
#                                ledger as flakes
#   flakes.sh entries WHERE COMMIT FAILED-AT LOG
#                                prints the ledger's entries for a failure in LOG, for CI, which
#                                finds a rerun's earlier failures itself
#
# The same code is the same tree, not the same commit: the gate merges main into what it tests,
# which makes a new commit each time even when nothing changed. The ledger is `flakes.log` in
# Fennec's folder, beside `work` rather than in it, since what's in `work` is swept after an hour.
# Each entry is the test, where it failed, the commit and tree, the machine's load then, when it
# failed and when it passed again, and what it printed when it failed:
#
#   === flake
#   test: cli::the_daemon_keeps_the_cache_under_its_cap
#   ...
#   --- what it printed
#   ...
#   === end
set -u
folder=${FENNEC_HOME:-$HOME/.fennec}
ledger="$folder/flakes.log"
pending="$folder/flakes-pending"

# The machine's load averages, as `uptime` gives them.
load() {
  uptime | sed -E 's/.*load averages?: *//'
}

# Prints one entry for each test that failed in LOG, with the part of LOG that's that test's own,
# or, where no test's name can be found, as when a step other than the tests failed, one entry for
# the step, with the end of LOG.
entries() {
  local where=$1 commit=$2 failed_at=$3 log=$4 passed_at=$5 tree=$6 load=$7
  local header="where: $where
commit: $commit
tree: $tree
load when it failed: $load
failed at: $failed_at
passed again at: $passed_at"
  # A Rust test's own output starts at `---- name stdout ----` and runs to the next one, or to
  # the list of failures after the last.
  if grep -q '^---- .* stdout ----$' "$log"; then
    # Through the environment: macOS's awk takes no newline in a value given with `-v`.
    HEADER="$header" awk '
      BEGIN { header = ENVIRON["HEADER"] }
      /^---- .* stdout ----$/ {
        if (name != "") print "=== end"
        name = $2
        print "=== flake"; print "test: " name; print header; print "--- what it printed"
        next
      }
      /^failures:$/ && name != "" { print "=== end"; name = ""; next }
      name != "" { print }
      END { if (name != "") print "=== end" }
    ' "$log"
    return
  fi
  # Otherwise the step, named by the command the gate says failed, or by its last line.
  local said
  # The backticks are the gate's own, in what it printed.
  # shellcheck disable=SC2016
  said=$(grep -m1 -E '^`fennec .*` (gave|was killed)|^::error::' "$log" || grep -v '^[[:space:]]*$' "$log" | tail -n 1)
  printf '=== flake\ntest: %s: %s\n%s\n--- what it printed\n' "$where" "$said" "$header"
  tail -n 200 "$log"
  echo "=== end"
}

tree_now() {
  git rev-parse 'HEAD^{tree}'
}

case "${1:-}" in
  failed)
    step=$2 log=$3
    tree=$(tree_now) || exit 1
    mkdir -p "$pending/$tree" || exit 1
    kept="$pending/$tree/$(date -u +%Y%m%dT%H%M%SZ)-$$"
    {
      echo "step: $step"
      echo "commit: $(git rev-parse HEAD)"
      echo "failed at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
      echo "load: $(load)"
      echo "---"
      cat "$log"
    } > "$kept.tmp" && mv "$kept.tmp" "$kept"
    ;;
  passed)
    tree=$(tree_now) || exit 1
    [ -d "$pending/$tree" ] || exit 0
    passed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    for kept in "$pending/$tree"/*; do
      [ -f "$kept" ] || continue
      step=$(sed -n 's/^step: //p' "$kept" | head -n 1)
      commit=$(sed -n 's/^commit: //p' "$kept" | head -n 1)
      failed_at=$(sed -n 's/^failed at: //p' "$kept" | head -n 1)
      was=$(sed -n 's/^load: //p' "$kept" | head -n 1)
      output=$(mktemp)
      sed '1,/^---$/d' "$kept" > "$output"
      entries "gate: $step" "$commit" "$failed_at" "$output" "$passed_at" "$tree" "$was" >> "$ledger"
      rm -f "$output" "$kept"
      echo "the gate failed on this code before, at \"$step\", and passed now: written to $ledger as a flake" >&2
    done
    rmdir "$pending/$tree" 2> /dev/null
    ;;
  entries)
    where=$2 commit=$3 failed_at=$4 log=$5
    entries "$where" "$commit" "$failed_at" "$log" "${PASSED_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}" "${TREE:-unknown}" "${LOAD:-unknown}"
    ;;
  *)
    echo "usage: flakes.sh failed STEP LOG | passed | entries WHERE COMMIT FAILED-AT LOG" >&2
    exit 2
    ;;
esac
