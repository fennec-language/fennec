//! Names the build of Fennec this is part of, the same `FENNEC_BUILD` the `fennec` command and
//! the runtime library are stamped with, so what the cache keeps belongs to one build: a change to
//! the compiler can never be hidden by what an older one compiled.

include!("../fennec/build_hash.rs");

fn main() {
    println!("cargo::rustc-env=FENNEC_BUILD={}", build_hash());
}
