#!/usr/bin/env bash
# Builds what tsan-programs.sh runs: `fennec` and the fuzzer for release, and beside them the
# runtime libraries, plain and with threads, built with ThreadSanitizer by the nightly compiler in
# NIGHTLY, as CI's sanitizer jobs build them. Linux on x86-64 or Arm.
set -euo pipefail
: "${NIGHTLY:?name the nightly compiler, as CI does}"
target="$(uname -m)-unknown-linux-gnu"
cargo build --release --locked -p fennec -p fennec-fuzz
# `sanitize`: the system's allocator, so the sanitizer sees each block.
RUSTFLAGS=-Zsanitizer=thread cargo "+$NIGHTLY" build --release --locked -p fennec-runtime \
  --features sanitize -Zbuild-std --target "$target" --target-dir target/tsan
cp "target/tsan/$target/release/libfennec_runtime.a" target/release/libfennec_runtime.a
RUSTFLAGS=-Zsanitizer=thread cargo "+$NIGHTLY" build --release --locked -p fennec-runtime \
  --features threads,sanitize -Zbuild-std --target "$target" --target-dir target/tsan-threads
cp "target/tsan-threads/$target/release/libfennec_runtime.a" target/release/libfennec_runtime_threads.a
