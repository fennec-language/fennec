//! Names this build of Fennec by a hash of the source of every crate, as `FENNEC_BUILD`. Which
//! tests passed and which programs checked is only worth remembering for the build that found
//! it out, and a version number says nothing while Fennec itself is changing. The runtime library
//! is stamped with the same name, so `fennec` can tell one built from other source.

include!("build_hash.rs");

fn main() {
    println!("cargo::rustc-env=FENNEC_BUILD={}", build_hash());
    // The command links fennec-rt for its layouts and constants, and with it the libraries a
    // program's requests need: on macOS, Security and CoreFoundation, whose loading costs about
    // 10M instructions every time the command starts. The command never makes a request, so it
    // uses nothing of them, and the linker leaves out a library nothing uses.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo::rustc-link-arg-bins=-Wl,-dead_strip_dylibs");
    }
}
