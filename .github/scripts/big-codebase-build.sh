#!/usr/bin/env bash
# Builds what `fennec-bench big-codebase` measures with, as the nightly and the weekly run it:
# `fennec`, its runtimes and the bench in release, and the runtimes' bitcode, which a release build
# puts what it uses of in the program's own unit. Stops at the first step that fails.
set -euo pipefail
cargo build --release --locked -p fennec -p fennec-runtime -p fennec-bench
cargo rustc --release --locked -p fennec-runtime --crate-type staticlib -- --emit=llvm-bc,link -C lto=fat -C codegen-units=1
cp "$(find target/release/deps -name 'fennec_runtime-*.bc' -print -quit)" target/release/libfennec_runtime.bc
cargo rustc --release --locked -p fennec-runtime --features threads --target-dir target/threads --crate-type staticlib -- --emit=llvm-bc,link -C lto=fat -C codegen-units=1
cp target/threads/release/libfennec_runtime.a target/release/libfennec_runtime_threads.a
cp "$(find target/threads/release/deps -name 'fennec_runtime-*.bc' -print -quit)" target/release/libfennec_runtime_threads.bc
