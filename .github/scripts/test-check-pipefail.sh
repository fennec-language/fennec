#!/usr/bin/env bash
# Tries `check-pipefail.sh` on a throwaway repository's workflows. Each pipe whose exit is lost has
# to be found: one that is a command of its own with no `pipefail` in force, as `fennec-fuzz run … |
# tee` was (273), and one in a substitution anywhere but all of an assignment with `pipefail`, as
# `[ "$(./checked | sort)" = … ]` was. Nothing else may be: a pipe the script tests itself, one
# where `pipefail` is set, a Windows job's, a `case` pattern, an `||` ending a pipe that reads
# quietly, and a `|` in quotes, `${{ }}` or a comment. A pipe kept with `|| status=$?` is found: its
# exit is tee's.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() {
  echo "FAILED: $*"
  failures=$((failures + 1))
}
mkdir -p "$work/.github/workflows" "$work/.github/actions/act"

# Every step here pipes as a command of its own without `pipefail`, each in its own way.
cat > "$work/.github/workflows/bad.yml" << 'EOF'
on: push
jobs:
  plain:
    runs-on: ubuntu-latest
    steps:
      - name: tee
        run: ./fuzz run | tee fuzz.txt
      - name: in a loop
        run: |
          for x in a b; do
            ./run "$x" | sort > printed.txt
          done
      - name: after then
        run: if [ -f x ]; then sort x | sha256sum; fi
      - name: carried on
        run: |
          ./fuzz run \
            | tee fuzz.txt
      - name: a shell of its own with no pipefail
        shell: bash -e {0}
        run: ./fuzz run | tee fuzz.txt
      - name: pipefail set too late
        run: |
          echo starting
          set -o pipefail
          ./fuzz run | tee fuzz.txt
      - name: an exit kept from tee
        run: |
          status=0
          ./fuzz run | tee fuzz.txt || status=$?
          exit "$status"
  matrix:
    runs-on: ${{ matrix.os }}
    steps:
      - name: on a runner that may not be Windows
        run: ./fuzz run | tee fuzz.txt
EOF
cat > "$work/.github/actions/act/action.yml" << 'EOF'
runs:
  using: composite
  steps:
    - name: an action's step
      shell: sh
      run: ./fuzz run | tee fuzz.txt
EOF
said=$("$here/check-pipefail.sh" "$work")
status=$?
[ $status -eq 1 ] || fail "pipes with no pipefail gave $status, not 1"
for step in "tee" "in a loop" "after then" "carried on" "a shell of its own with no pipefail" \
  "pipefail set too late" "an exit kept from tee" "on a runner that may not be Windows" "an action's step"; do
  grep -qF "\"$step\" pipes with no pipefail" <<< "$said" || fail "the step \"$step\" wasn't found: $said"
done
[ "$(grep -c '::error::' <<< "$said")" -eq 9 ] || fail "more than the 9 pipes were found: $said"

# Every step here pipes inside a substitution whose exit is lost, each in its own way, all but
# the last with `pipefail`.
/bin/rm -f "$work/.github/workflows/bad.yml" "$work/.github/actions/act/action.yml"
cat > "$work/.github/workflows/substituted.yml" << 'EOF'
on: push
jobs:
  substituted:
    runs-on: ubuntu-latest
    defaults:
      run:
        shell: bash
    steps:
      - name: in a test
        run: '[ "$(./checked | sort)" = "$(cat wanted.txt)" ]'
      - name: in an argument
        run: echo "printed $(./checked | sort)"
      - name: in a local
        run: |
          read_it() {
            local got=$(./checked | sort)
          }
      - name: in a process substitution
        run: sha256sum < <(./checked | sort)
      - name: inside an assignment's argument
        run: got=$(sha256sum "$(./checked | sort)")
      - name: in backquotes in a test
        run: '[ "`./checked | sort`" = x ]'
      - name: an assignment with no pipefail
        shell: bash -e {0}
        run: got=$(./checked | sort)
EOF
said=$("$here/check-pipefail.sh" "$work")
status=$?
[ $status -eq 1 ] || fail "pipes in substitutions gave $status, not 1"
for step in "in a test" "in an argument" "in a local" "in a process substitution" \
  "inside an assignment's argument" "in backquotes in a test" "an assignment with no pipefail"; do
  grep -qF "\"$step\" pipes inside" <<< "$said" || fail "the step \"$step\" wasn't found: $said"
done
[ "$(grep -c '::error::' <<< "$said")" -eq 7 ] || fail "more than the 7 pipes were found: $said"

# None of these is a pipe whose exit is lost.
/bin/rm -f "$work/.github/workflows/substituted.yml"
cat > "$work/.github/workflows/good.yml" << 'EOF'
on: push
defaults:
  run:
    shell: bash
jobs:
  by-the-workflow:
    runs-on: ubuntu-latest
    steps:
      - run: ./fuzz run | tee fuzz.txt
      - name: all of an assignment, with pipefail
        run: |
          got=$(./checked | sort | sha256sum)
          dir="target/$(echo "$features" | tr , -)"
          got=$(./checked | sort) || status=$?
          when=$(echo "$(date)")
          [ "$(cat x)" = y ]
EOF
cat > "$work/.github/workflows/tested.yml" << 'EOF'
on: push
jobs:
  tested:
    runs-on: ubuntu-latest
    defaults:
      run:
        shell: bash -eo pipefail {0}
    steps:
      - run: ./fuzz run | tee fuzz.txt
  own:
    runs-on: ubuntu-latest
    steps:
      - name: the script's own
        run: |
          # It sets it first.
          set -euo pipefail
          ./fuzz run | tee fuzz.txt
      - name: the step's own
        shell: bash
        run: ./fuzz run | tee fuzz.txt
      - name: tested by the script
        run: |
          if ! nm checked | grep -q symbol; then exit 1; fi
          while ./next | grep -q more; do :; done
          ! ./run 2>&1 | grep -q "a message"
          nm checked | grep -q "$SYMBOL" || failed=1
          ./gen | cmp -s - want || echo differs
          sha256sum < <(sort x)
          case "$x" in push | schedule) ;; *) exit 1 ;; esac
          case "$status" in
            0 | 2 | 3) ;;
          esac
          [ -s x ] || echo "a | b" ; echo 'c | d'
          echo "${{ github.event_name == 'push' || 'x' }}"
          # what | it does
  windows:
    runs-on: windows-latest
    steps:
      - run: Get-Content x | Select-Object -First 1
EOF
said=$("$here/check-pipefail.sh" "$work")
status=$?
[ $status -eq 0 ] || fail "a pipe that needs no pipefail was found, giving $status: $said"

[ $failures -eq 0 ] && echo "check-pipefail.sh finds each pipe whose exit is lost, and nothing else"
exit $failures
