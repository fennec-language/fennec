# Third-party software in a Fennec archive

Fennec itself is under the licenses in `LICENSE-MIT` and `LICENSE-APACHE`, and its runtime library and standard library add the exception in `RUNTIME-LIBRARY-EXCEPTION`. An archive also carries the software below, unchanged, under its own licenses. Their texts are in the `licenses` folder. None of it is under the GPL.

## In the `llvm` folder

`opt`, `llc` and `llvm-link` in `llvm/bin`, and LLVM's shared library in `llvm/lib`. `fennec build` and `fennec run` run them to compile programs. Nothing of them is built into a program.

They're from the [LLVM Project](https://llvm.org), as the Rust project builds them for its `llvm-tools` component. They're under the Apache License 2.0 with LLVM Exceptions (`licenses/Apache-2.0.txt` and `licenses/LLVM-exception.txt`) and the University of Illinois/NCSA Open Source License (`licenses/NCSA.txt`). The Rust project's copyright notices for what it ships, LLVM among them, are in `licenses/rust-COPYRIGHT.html`.
