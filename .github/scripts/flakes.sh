#!/usr/bin/env bash
# The flake ledger. A check that fails and then passes on the same code is a flake, and every one
# is written down, so nothing is ever "just rerun": the coverage lane reads the ledger nightly, and
# `fennec-flakes` lists it by test.
#
#   flakes.sh failed STEP LOG    a gate step failed, printing LOG: it's kept, pending, under the
#                                code it tested
#   flakes.sh passed             the gate passed: what failed before on the same code goes in the
#                                ledger as flakes, or as fixed on main (below)
#   flakes.sh entries WHERE COMMIT FAILED-AT LOG
#                                prints the ledger's entries for a failure in LOG, for CI, which
#                                finds a rerun's earlier failures itself
#
# The same change is the same tree, or the same branch with the same commits of its own: the gate
# merges main into what it tests, which makes a new commit each time, and a new tree whenever main
# moved, though the branch's change is the same. The ledger is `flakes.log` in
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
#
# A failure that passed with only main moved is a flake only when main's new commits changed
# nothing it tests. When they changed the failing test's own file, or the compiler and runtime
# crates every test runs through (`check`, `ir`, `llvm`, `rt`, `runtime`, `syntax`), main may
# well have fixed it: a red test on main, fixed there, passes in the next gate of every branch.
# Then it goes in `fixed-on-main.log`, beside the ledger, as `=== fixed on main`, naming what main
# changed, and not in the ledger.
set -u
folder=${FENNEC_HOME:-$HOME/.fennec}
ledger="$folder/flakes.log"
fixed_on_main="$folder/fixed-on-main.log"
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

# What main changed, between the commit that failed and this one, that could have fixed the
# failure in LOG: the compiler and runtime crates, each failing test's own file, which is where
# it panicked and the test file its first module is named for (`llvm::a_loop` is in
# `crates/*/tests/llvm.rs`), and the code of that file's crate, which its tests run (#2535 fixed
# `crates/fuzz/tests/kept.rs`'s tests in `crates/fuzz/src`). Prints each such path. It prints none when the commit that failed
# can't be read, and the failure stays a flake, as before.
touched_by_main() {
  local failed=$1 log=$2 changed files code path module
  changed=$(git diff --name-only "$failed" HEAD 2> /dev/null) || return 0
  [ -n "$changed" ] || return 0
  files=$(
    sed -n -E 's/.*panicked at ([^:]+):[0-9]+:[0-9]+.*/\1/p' "$log"
    sed -n -E 's/^---- ([A-Za-z0-9_]+)::.* stdout ----$/\1/p' "$log" | sort -u | while IFS= read -r module; do
      printf '%s\n' "$changed" | grep -E "^crates/[^/]+/tests/$module(\.rs|/)" || true
    done
  )
  code=$(printf '%s\n' "$files" | sed -n -E 's#^(crates/[^/]+)/.*#\1/src/#p' | sort -u)
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    case "$path" in
      crates/check/* | crates/ir/* | crates/llvm/* | crates/rt/* | crates/runtime/* | crates/syntax/*)
        echo "$path"
        ;;
      *)
        if printf '%s\n' "$files" | grep -qxF -- "$path"; then
          echo "$path"
        else
          # Crate folders hold no spaces.
          for dir in $code; do
            case "$path" in "$dir"*)
              echo "$path"
              break
              ;;
            esac
          done
        fi
        ;;
    esac
  done <<< "$changed"
}

# The branch a gate tests, and a hash of its own commits: every one not on main that isn't a merge.
# Each gate merges main first, so two gates of a branch whose own commits are the same test the
# same change, whatever main brought in between.
branch_now() {
  git rev-parse --abbrev-ref HEAD
}
own_now() {
  local base=origin/main
  git rev-parse --verify --quiet "$base" > /dev/null || base=main
  git log --no-merges --format=%H "$base..HEAD" | git hash-object --stdin
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
      echo "branch: $(branch_now)"
      echo "own: $(own_now)"
      echo "failed at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
      echo "load: $(load)"
      echo "---"
      cat "$log"
    } > "$kept.tmp" && mv "$kept.tmp" "$kept"
    ;;
  passed)
    # A failure kept before is a flake when this pass tested the same change: the same tree, or the
    # same branch with the same commits of its own, where only main moved in between, as it does
    # each time the gate merges it. One from a branch whose own commits changed since was fixed or
    # changed, not shown to be a flake, and goes; one from another branch stays for that branch.
    tree=$(tree_now) || exit 1
    branch=$(branch_now)
    own=$(own_now)
    passed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    [ -d "$pending" ] || exit 0
    for kept in "$pending"/*/*; do
      [ -f "$kept" ] || continue
      failed_tree=$(basename "$(dirname "$kept")")
      kept_branch=$(sed -n 's/^branch: //p' "$kept" | head -n 1)
      kept_own=$(sed -n 's/^own: //p' "$kept" | head -n 1)
      main_moved=false
      if [ "$failed_tree" = "$tree" ]; then
        shown_tree=$tree
      elif [ -n "$kept_branch" ] && [ "$kept_branch" = "$branch" ] && [ "$kept_own" = "$own" ]; then
        shown_tree="$failed_tree, passed on $tree with only main moved"
        main_moved=true
      elif [ -n "$kept_branch" ] && [ "$kept_branch" = "$branch" ]; then
        rm -f "$kept"
        rmdir "$(dirname "$kept")" 2> /dev/null
        continue
      else
        continue
      fi
      step=$(sed -n 's/^step: //p' "$kept" | head -n 1)
      commit=$(sed -n 's/^commit: //p' "$kept" | head -n 1)
      failed_at=$(sed -n 's/^failed at: //p' "$kept" | head -n 1)
      was=$(sed -n 's/^load: //p' "$kept" | head -n 1)
      output=$(mktemp)
      sed '1,/^---$/d' "$kept" > "$output"
      fixed=""
      if [ "$main_moved" = true ]; then
        fixed=$(touched_by_main "$commit" "$output" | head -n 10 | paste -s -d ' ' -)
      fi
      if [ -n "$fixed" ]; then
        entries "gate: $step" "$commit" "$failed_at" "$output" "$passed_at" \
          "$failed_tree, passed on $tree after main changed $fixed" "$was" |
          sed 's/^=== flake$/=== fixed on main/' >> "$fixed_on_main"
        echo "the gate failed on this change before, at \"$step\", and passed now, after main changed what it tests ($fixed): written to $fixed_on_main as fixed on main, not a flake" >&2
      else
        entries "gate: $step" "$commit" "$failed_at" "$output" "$passed_at" "$shown_tree" "$was" >> "$ledger"
        echo "the gate failed on this change before, at \"$step\", and passed now: written to $ledger as a flake" >&2
      fi
      rm -f "$output" "$kept"
      rmdir "$(dirname "$kept")" 2> /dev/null
    done
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
