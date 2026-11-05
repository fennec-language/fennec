# Third-party software in a Fennec archive

Fennec itself is under the licenses in `LICENSE-MIT` and `LICENSE-APACHE`. An archive also carries these, unchanged, under their own licenses. Their texts are in the `licenses` folder, and none of them is under the GPL.

## In the `llvm` folder, on every platform

`opt`, `llc`, `llvm-link`, and LLVM's shared library where the platform has one. On Windows, also `llvm-ar` and `rust-lld`, which is LLD.

They're from the [LLVM Project](https://github.com/rust-lang/llvm-project), built by the Rust project, under the Apache License 2.0 with LLVM Exceptions and the University of Illinois/NCSA license, in `licenses/LLVM.txt` on Windows.

## In `llvm/lib/self-contained`, on Windows

These come from [llvm-mingw](https://github.com/mstorsjo/llvm-mingw).

- **LLVM's `libunwind` and `compiler-rt` builtins:** the Apache License 2.0 with LLVM Exceptions, in `licenses/LLVM.txt`. The exceptions cover what's linked into a program.
- **MinGW-w64's runtime and Windows import libraries:** the notices in `licenses/mingw-w64-runtime.txt`. Parts of the runtime are linked into every program `fennec build` makes on Windows, and these notices ask to be included when such a program is distributed in binary form.
