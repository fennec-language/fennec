//! Names this build of Fennec by a hash of the source of every crate, as `FENNEC_BUILD`. Which
//! tests passed and which programs checked is only worth remembering for the build that found
//! it out, and a version number says nothing while Fennec itself is changing. The runtime library
//! is stamped with the same name, so `fennec` can tell one built from other source.

include!("build_hash.rs");

fn main() {
    println!("cargo::rustc-env=FENNEC_BUILD={}", build_hash());
}
