# Third-party software in a Fennec archive

Fennec itself is under the licenses in `LICENSE-MIT` and `LICENSE-APACHE`. An archive also carries these, unchanged, as Rust's toolchain ships them, under their own licenses.

## In the `llvm` folder, on every platform

`opt`, `llc`, `llvm-link`, and LLVM's shared library where the platform has one. On Windows, also `llvm-ar` and `rust-lld`, which is LLD.

They're from the [LLVM Project](https://github.com/rust-lang/llvm-project), under the Apache License 2.0 with LLVM Exceptions and the University of Illinois/NCSA license. The project's `llvm/LICENSE.TXT` and `lld/LICENSE.TXT` have the full terms.

## In `llvm/lib/self-contained`, on Windows

MinGW's libraries, from the build Rust's toolchain bundles.

- **MinGW-w64's runtime and Windows import libraries:** [its notices](https://github.com/mingw-w64/mingw-w64/blob/master/COPYING.MinGW-w64-runtime/COPYING.MinGW-w64-runtime.txt). Parts of them are linked into the programs `fennec build` makes, and those notices ask to be included when such a program is distributed.
- **winpthreads:** the MIT license and a BSD-style license from Lockless Inc.: [its terms](https://github.com/mingw-w64/mingw-w64/blob/master/mingw-w64-libraries/winpthreads/COPYING).
- **GCC's `libgcc` and `libgcc_eh`:** the GNU General Public License version 3 or later with the [GCC Runtime Library Exception](https://www.gnu.org/licenses/gcc-exception-3.1.html). Their source is GCC's, at [gcc.gnu.org](https://gcc.gnu.org/).
