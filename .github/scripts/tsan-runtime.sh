#!/usr/bin/env bash
# Builds what tsan-programs.sh runs: `fennec` and the fuzzer for release, and beside them the
# runtime libraries, plain and with threads, built with ThreadSanitizer by the nightly compiler in
# NIGHTLY, as CI's sanitizer jobs build them. Linux on x86-64 or Arm.
set -euo pipefail
: "${NIGHTLY:?name the nightly compiler, as CI does}"
target="$(uname -m)-unknown-linux-gnu"
cargo build --release --locked -p fennec -p fennec-fuzz
# `sanitize`: the system's allocator, so the sanitizer sees each block. `crossbeam_sanitize_thread`:
# crossbeam's deques publish a job with a fence and a relaxed store, which ThreadSanitizer can't
# see, and take this to use a release store it can, as crossbeam asks of a ThreadSanitizer build.
# Without it, a worker stealing a job another just pushed is reported as a race (fennec#1990).
flags="-Zsanitizer=thread --cfg crossbeam_sanitize_thread"
RUSTFLAGS="$flags" cargo "+$NIGHTLY" build --release --locked -p fennec-runtime \
  --features sanitize -Zbuild-std --target "$target" --target-dir target/tsan
cp "target/tsan/$target/release/libfennec_runtime.a" target/release/libfennec_runtime.a
RUSTFLAGS="$flags" cargo "+$NIGHTLY" build --release --locked -p fennec-runtime \
  --features threads,sanitize -Zbuild-std --target "$target" --target-dir target/tsan-threads
cp "target/tsan-threads/$target/release/libfennec_runtime.a" target/release/libfennec_runtime_threads.a
