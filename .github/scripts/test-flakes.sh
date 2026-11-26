#!/usr/bin/env bash
# Tries `flakes.sh` on a throwaway repository, as the gate runs it: a gate of a change fails, main
# may move, and the next gate of the change passes. What failed is a flake when it passed on the
# same tree, or when main changed nothing it tests. It's fixed on main, and kept out of the ledger,
# when main changed the compiler's crates, the failing test's own file or its crate's code.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fail() {
  echo "test-flakes: $1" >&2
  exit 1
}

export FENNEC_HOME="$work/home"
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com
ledger="$FENNEC_HOME/flakes.log"
fixed="$FENNEC_HOME/fixed-on-main.log"
mkdir -p "$FENNEC_HOME"

repo="$work/repo"
git init -q -b main "$repo"
cd "$repo"
mkdir -p crates/llvm/src crates/fennec/src crates/fennec/tests crates/other/src docs
echo start > crates/llvm/src/lib.rs
echo start > crates/fennec/src/lib.rs
echo start > crates/other/src/lib.rs
echo start > crates/fennec/tests/llvm.rs
echo start > docs/notes.md
git add -A
git commit -q -m start
git checkout -q -b change
echo mine > mine.txt
git add mine.txt
git commit -q -m "the change"

# A test's failure, as cargo prints it.
failure="$work/failed.log"
cat > "$failure" << 'EOF'
---- llvm::a_loop_makes_no_slice stdout ----
thread 'llvm::a_loop_makes_no_slice' (4242) panicked at crates/fennec/tests/llvm.rs:3:5:
it made a slice
failures:
    llvm::a_loop_makes_no_slice
EOF

# Main gets a commit changing PATH, and the change merges it, as the next gate does.
moved=0
main_changes() {
  moved=$((moved + 1))
  git checkout -q main
  echo "$moved" >> "$1"
  git commit -q -am "main changes $1"
  git checkout -q change
  git merge -q --no-edit main
}

# One round: a gate fails, main changes PATH (or nothing, for "-"), and a gate passes.
round() {
  "$here/flakes.sh" failed tests "$failure"
  if [ "$1" != - ]; then
    main_changes "$1"
  fi
  "$here/flakes.sh" passed 2> "$work/said"
}

count() {
  if [ -f "$2" ]; then grep -c "^=== $1\$" "$2" || true; else echo 0; fi
}
expect() {
  local flakes fixes
  flakes=$(count flake "$ledger")
  fixes=$(count "fixed on main" "$fixed")
  [ "$flakes" = "$1" ] && [ "$fixes" = "$2" ] ||
    fail "$3: $flakes flakes and $fixes fixed on main, not $1 and $2: $(cat "$work/said")"
}

round docs/notes.md
expect 1 0 "main changed only the docs, so it's a flake"
grep -q "written to $ledger as a flake" "$work/said" || fail "a flake isn't said to be one: $(cat "$work/said")"

round crates/llvm/src/lib.rs
expect 1 1 "main changed a compiler crate, so it's fixed on main"
grep -q "after main changed crates/llvm/src/lib.rs" "$fixed" || fail "the entry doesn't name what main changed: $(cat "$fixed")"
grep -q "^test: llvm::a_loop_makes_no_slice$" "$fixed" || fail "the entry doesn't name the test: $(cat "$fixed")"
grep -q "as fixed on main, not a flake" "$work/said" || fail "it isn't said to be fixed on main: $(cat "$work/said")"

round crates/fennec/tests/llvm.rs
expect 1 2 "main changed the failing test's own file, so it's fixed on main"

round crates/fennec/src/lib.rs
expect 1 3 "main changed the code of the failing test's crate, so it's fixed on main"
grep -q "after main changed crates/fennec/src/lib.rs" "$fixed" || fail "the entry doesn't name the crate's code: $(cat "$fixed")"

round crates/other/src/lib.rs
expect 2 3 "main changed only another crate's code, so it's a flake"

round -
expect 3 3 "it passed on the same tree, so it's a flake"

[ -z "$(ls -A "$FENNEC_HOME/flakes-pending" 2> /dev/null)" ] || fail "failures were left pending"
echo "test-flakes: passed"
