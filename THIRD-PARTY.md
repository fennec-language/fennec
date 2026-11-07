# Third-party software in a Fennec archive

Fennec itself is under the licenses in `LICENSE-MIT` and `LICENSE-APACHE`, and its runtime library and standard library add the exception in `RUNTIME-LIBRARY-EXCEPTION`. An archive also carries the software below, unchanged, under its own licenses. Their texts are in the `licenses` folder. None of it is under the GPL.

## In the `llvm` folder

`opt`, `llc` and `llvm-link` in `llvm/bin`, and LLVM's shared library in `llvm/lib` where the platform has one. On Windows, also `llvm-ar` and `rust-lld`, which is LLD. `fennec build` and `fennec run` run them to compile and link programs. Nothing of them is built into a program.

They're from the [LLVM Project](https://llvm.org), as the Rust project builds them for its `llvm-tools` component. They're under the Apache License 2.0 with LLVM Exceptions (`licenses/Apache-2.0.txt` and `licenses/LLVM-exception.txt`) and the University of Illinois/NCSA Open Source License (`licenses/NCSA.txt`). The Rust project's copyright notices for what it ships, LLVM among them, are in `licenses/rust-COPYRIGHT.html`.

## In `llvm/lib/self-contained`, on Windows

What LLD links into every program on Windows, from [llvm-mingw](https://github.com/mstorsjo/llvm-mingw).

- **LLVM's `libunwind` and `compiler-rt` builtins:** the Apache License 2.0 with LLVM Exceptions, as above. The exceptions cover what's linked into a program.
- **MinGW-w64's runtime and Windows import libraries:** the notices in `licenses/mingw-w64-runtime.txt`. Parts of the runtime are linked into every program `fennec build` makes on Windows, and these notices ask to be included when such a program is distributed in binary form, so on Windows `RUNTIME-LIBRARY-EXCEPTION` doesn't cover the whole of a program.

## In the `musl` folder, on Linux

What a release build links into a program on Linux, so the program runs with nothing from the machine it runs on: no C library and no loader. They're from the Rust project's `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` targets, and the runtime library beside them is built for those targets.

- **musl's C library and start-up files:** the MIT license, in `licenses/musl-copyright.txt`. They're linked into every program `fennec build --release` makes on Linux, and the license asks for its notice to be included with copies, so on Linux `RUNTIME-LIBRARY-EXCEPTION` doesn't cover the whole of such a program. `FENNEC_STATIC=0` links one against the system's C library instead.
- **LLVM's `libunwind`:** the Apache License 2.0 with LLVM Exceptions, as above. The exceptions cover what's linked into a program.
