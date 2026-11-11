#!/usr/bin/env bash
# Fails, naming them, if `fennec-*` folders are left in the system's temporary folder, the one
# Rust's `std::env::temp_dir` gives: every test removes what it makes there however it ends
# (see crates/temp), and a run that leaves folders behind fills the disk a few at a time. The
# fuzz harness's `fennec-fuzz-*` folders are its own to tidy.
set -euo pipefail
if command -v cygpath > /dev/null && [ -n "${TMP:-${TEMP:-}}" ]; then
  temp=$(cygpath -u "${TMP:-$TEMP}")
else
  temp=${TMPDIR:-/tmp}
fi
left=$(find "$temp" -mindepth 1 -maxdepth 1 -type d -name 'fennec-*' ! -name 'fennec-fuzz-*' | sort)
if [ -n "$left" ]; then
  echo "::error::the tests left these folders in $temp:"
  echo "$left"
  exit 1
fi
echo "no fennec-* folders left in $temp"
