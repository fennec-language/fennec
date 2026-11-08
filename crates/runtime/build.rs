//! Stamps the runtime library with the build of Fennec it's from, the same `FENNEC_BUILD` the
//! `fennec` command is built with, so `fennec` can refuse to link programs with a runtime library
//! built from other source, which would fail in ways that say nothing about why.

include!("../fennec/build_hash.rs");

fn main() {
    println!("cargo::rustc-env=FENNEC_BUILD={}", build_hash());
}
